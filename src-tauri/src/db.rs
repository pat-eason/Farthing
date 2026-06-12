//! SQLite persistence layer.
//!
//! Owns the `usage.db` database (WAL mode) and its embedded, versioned
//! migrations. The schema version is tracked in the `meta` table under the
//! `schema_version` key; migrations are idempotent across restarts and only
//! statements newer than the stored version are applied.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

/// File name of the database inside the app data directory.
pub const DB_FILE_NAME: &str = "usage.db";

/// Directory name (under `~/Library/Application Support/`) used by the
/// pre-Farthing bundle identifier. Source of the one-time rename migration
/// (see [`migrate_legacy_data_dir`]).
pub const LEGACY_DATA_DIR_NAME: &str = "com.peason.claude-usage-tracker";

/// Key in `meta` that stores the current schema version.
const SCHEMA_VERSION_KEY: &str = "schema_version";

/// Embedded migrations. Index 0 is schema version 1, index 1 is version 2,
/// and so on. Existing entries must never be edited once shipped; schema
/// changes are appended as new entries.
const MIGRATIONS: &[&str] = &[
    // v1: initial schema — requests, sessions, ingest_state.
    "
    CREATE TABLE requests (
        id INTEGER PRIMARY KEY,
        request_id TEXT,
        session_id TEXT,
        timestamp_ms INTEGER NOT NULL,
        model TEXT,
        query_source TEXT,
        cost_usd REAL,
        input_tokens INTEGER NOT NULL DEFAULT 0,
        output_tokens INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens INTEGER NOT NULL DEFAULT 0,
        cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
        cache_creation_5m_tokens INTEGER,
        cache_creation_1h_tokens INTEGER,
        event_type TEXT NOT NULL DEFAULT 'api_request',
        error TEXT,
        duration_ms INTEGER,
        source TEXT NOT NULL DEFAULT 'otel' CHECK (source IN ('otel', 'backfill'))
    );

    CREATE INDEX idx_requests_timestamp ON requests (timestamp_ms);
    CREATE INDEX idx_requests_session_id ON requests (session_id);
    CREATE INDEX idx_requests_model ON requests (model);

    CREATE TABLE sessions (
        session_id TEXT PRIMARY KEY,
        cwd TEXT,
        first_seen_ms INTEGER NOT NULL,
        last_seen_ms INTEGER,
        source TEXT NOT NULL DEFAULT 'hook' CHECK (source IN ('hook', 'backfill'))
    ) WITHOUT ROWID;

    CREATE TABLE ingest_state (
        file_path TEXT PRIMARY KEY,
        byte_offset INTEGER NOT NULL DEFAULT 0,
        updated_at_ms INTEGER NOT NULL
    ) WITHOUT ROWID;
    ",
    // v2: dedup identity (spike 3.1, docs/notes/dedup-key.md) — `request_id`
    // is the exact dedup key across otel and backfill rows, enforced by a
    // partial unique index. Pre-existing duplicates (an OTLP re-delivery
    // could have inserted the same request twice under v1) are collapsed to
    // the earliest row first so the index can build; v1 rows are all
    // source='otel' so no preference beyond first-wins is needed.
    "
    DELETE FROM requests
     WHERE request_id IS NOT NULL
       AND id NOT IN (
           SELECT MIN(id) FROM requests
            WHERE request_id IS NOT NULL
            GROUP BY request_id
       );

    CREATE UNIQUE INDEX idx_requests_request_id
        ON requests (request_id)
        WHERE request_id IS NOT NULL;
    ",
    // v3: covering index for time-windowed rollups (popover today-metrics,
    // task 4.2; the 4.3 sparkline and Epic 5 cost-over-time queries hit the
    // same shape). With only `(timestamp_ms)` indexed, every row in the
    // window costs a main-table probe (~58ms for a 15k-request day at 120k
    // total rows; the popover budget is <100ms). This index makes the
    // rollup queries index-only scans (~1ms). `idx_requests_timestamp` is
    // dropped: its key is the leftmost prefix of this one, so every query
    // it served is served at least as well here.
    "
    DROP INDEX idx_requests_timestamp;

    CREATE INDEX idx_requests_time_rollup ON requests (
        timestamp_ms, session_id, event_type, cost_usd,
        input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens
    );
    ",
    // v4: faceted-query covering index (task 5.2). The Epic 5 analysis
    // views filter every rollup by model and query_source on top of the
    // time window, and the tokens view reads the 5m/1h cache-creation
    // split; none of those columns are in the v3 index, so each faceted
    // row would cost a main-table probe. The replacement keeps the v3
    // column order as a prefix (every query the v3 index served is served
    // at least as well) and appends the facet/split columns so all Epic 5
    // aggregations stay index-only scans.
    //
    // The session-leading twin serves the per-session/per-project rollups
    // (GROUP BY session_id): scanning it in index order aggregates with no
    // sorter pass, which is what keeps those rollups inside the Epic 5
    // <500ms budget at 1M rows (a time-leading scan + sort measured ~590ms;
    // index-ordered grouping ~260ms). `idx_requests_session_id` is dropped:
    // its key is the leftmost prefix of the new index.
    //
    // `DELETE FROM ingest_state` is a one-time data heal, not a schema
    // change: backfill rows written before v4 never recorded
    // `query_source` (the transcript's sidechain flag is the authoritative
    // subagent marker). Resetting the per-file offsets makes the next
    // startup backfill pass re-read every transcript; request_id dedup
    // keeps the re-read idempotent, and the dedup path fills the missing
    // `query_source` on existing rows.
    "
    DROP INDEX idx_requests_time_rollup;

    CREATE INDEX idx_requests_facet_rollup ON requests (
        timestamp_ms, session_id, event_type, cost_usd,
        input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
        model, query_source, cache_creation_5m_tokens, cache_creation_1h_tokens
    );

    DROP INDEX idx_requests_session_id;

    CREATE INDEX idx_requests_session_rollup ON requests (
        session_id, timestamp_ms, event_type, cost_usd,
        input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
        model, query_source, cache_creation_5m_tokens, cache_creation_1h_tokens
    );

    DELETE FROM ingest_state;
    ",
];

/// Errors from opening or migrating the database.
#[derive(Debug)]
pub enum DbError {
    /// Failed to create the directory that should hold the database file.
    CreateDir(PathBuf, std::io::Error),
    /// Underlying SQLite error.
    Sqlite(rusqlite::Error),
    /// The on-disk schema version is newer than this binary understands
    /// (e.g. the user downgraded the app). Refuse to touch the database.
    FutureSchema { found: u64, supported: u64 },
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::CreateDir(path, err) => {
                write!(f, "failed to create directory {}: {err}", path.display())
            }
            DbError::Sqlite(err) => write!(f, "sqlite error: {err}"),
            DbError::FutureSchema { found, supported } => write!(
                f,
                "database schema version {found} is newer than supported version {supported}; \
                 refusing to open"
            ),
        }
    }
}

impl std::error::Error for DbError {}

impl From<rusqlite::Error> for DbError {
    fn from(err: rusqlite::Error) -> Self {
        DbError::Sqlite(err)
    }
}

/// Handle to the opened, migrated database.
pub struct Db {
    conn: Connection,
}

/// Tauri-managed wrapper. `rusqlite::Connection` is `Send` but not `Sync`,
/// so shared app state goes through a mutex; the `Arc` lets the ingest
/// pipeline (axum task) share the same handle.
pub struct DbState(pub Arc<Mutex<Db>>);

impl Db {
    /// Open (creating if necessary) `usage.db` inside `dir`, creating the
    /// directory itself if missing, then configure pragmas and migrate.
    pub fn open_in_dir(dir: &Path) -> Result<Self, DbError> {
        std::fs::create_dir_all(dir).map_err(|err| DbError::CreateDir(dir.to_path_buf(), err))?;
        Self::open(&dir.join(DB_FILE_NAME))
    }

    /// Open (creating if necessary) the database at `path`, configure
    /// pragmas, and apply any pending migrations.
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        configure_connection(&conn)?;
        apply_migrations(&conn, MIGRATIONS)?;
        Ok(Self { conn })
    }

    /// Current schema version as recorded in `meta`.
    pub fn schema_version(&self) -> Result<u64, DbError> {
        schema_version(&self.conn)
    }

    /// Active journal mode (expected: `wal`).
    pub fn journal_mode(&self) -> Result<String, DbError> {
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        Ok(mode)
    }

    /// Borrow the underlying connection for queries.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

/// One-time data migration for the Farthing rename (2026-06-12): the bundle
/// identifier change moved `app_data_dir`, leaving existing installs' data
/// behind in the old directory. Moves `usage.db` plus its `-wal`/`-shm`
/// siblings from `old_dir` into `new_dir` when — and only when — `new_dir`
/// has no `usage.db` of its own.
///
/// Non-destructive by construction:
/// - If `new_dir` already holds a `usage.db`, nothing happens (the new DB
///   wins; the old directory is left untouched).
/// - Files are *moved* (`rename`), never copied, so no stale second copy is
///   left behind to drift.
/// - The WAL/SHM siblings move before the main file: if the process dies
///   midway, the next start still sees no `usage.db` in `new_dir`, re-runs
///   the migration, and reunites the database with its already-moved WAL.
///
/// Returns `Ok(true)` when a database was moved, `Ok(false)` for a no-op.
pub fn migrate_legacy_data_dir(old_dir: &Path, new_dir: &Path) -> std::io::Result<bool> {
    if new_dir.join(DB_FILE_NAME).exists() {
        return Ok(false);
    }
    let old_db = old_dir.join(DB_FILE_NAME);
    if !old_db.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(new_dir)?;
    for suffix in ["-wal", "-shm"] {
        let sibling = old_dir.join(format!("{DB_FILE_NAME}{suffix}"));
        if sibling.exists() {
            std::fs::rename(&sibling, new_dir.join(format!("{DB_FILE_NAME}{suffix}")))?;
        }
    }
    std::fs::rename(&old_db, new_dir.join(DB_FILE_NAME))?;
    Ok(true)
}

fn configure_connection(conn: &Connection) -> Result<(), rusqlite::Error> {
    // WAL survives crashes and lets reads proceed during writes; NORMAL
    // synchronous is the recommended pairing for WAL.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // The ingest path and UI queries share this file; wait instead of
    // failing fast on transient contention.
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(())
}

/// Apply every migration in `migrations` whose version (index + 1) is
/// greater than the version stored in `meta`. Each migration runs in its
/// own transaction together with the version bump, so a failed migration
/// leaves the database at the previous consistent version.
fn apply_migrations(conn: &Connection, migrations: &[&str]) -> Result<(), DbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        ) WITHOUT ROWID;",
    )?;

    let current = schema_version(conn)?;
    let supported = migrations.len() as u64;
    if current > supported {
        return Err(DbError::FutureSchema {
            found: current,
            supported,
        });
    }

    for (index, sql) in migrations.iter().enumerate() {
        let version = (index + 1) as u64;
        if version <= current {
            continue;
        }
        conn.execute_batch("BEGIN")?;
        let result = conn.execute_batch(sql).and_then(|()| {
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                (SCHEMA_VERSION_KEY, version.to_string()),
            )
            .map(|_| ())
        });
        match result {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(err.into());
            }
        }
    }
    Ok(())
}

fn schema_version(conn: &Connection) -> Result<u64, DbError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [SCHEMA_VERSION_KEY],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok(value.and_then(|v| v.parse().ok()).unwrap_or(0))
}

#[cfg(test)]
impl fmt::Debug for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Db").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
    }

    fn index_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' ORDER BY name")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
    }

    #[test]
    fn fresh_create_makes_file_wal_and_full_schema() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_in_dir(dir.path()).unwrap();

        assert!(dir.path().join(DB_FILE_NAME).exists());
        assert_eq!(db.journal_mode().unwrap(), "wal");
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as u64);

        let tables = table_names(db.conn());
        for table in ["requests", "sessions", "ingest_state", "meta"] {
            assert!(tables.iter().any(|t| t == table), "missing table {table}");
        }

        let indexes = index_names(db.conn());
        for index in [
            "idx_requests_facet_rollup",
            "idx_requests_session_rollup",
            "idx_requests_model",
            "idx_requests_request_id",
        ] {
            assert!(indexes.iter().any(|i| i == index), "missing index {index}");
        }
        for replaced in ["idx_requests_time_rollup", "idx_requests_session_id"] {
            assert!(
                !indexes.iter().any(|i| i == replaced),
                "v4 must replace {replaced}"
            );
        }
    }

    #[test]
    fn v4_swaps_rollup_index_and_resets_backfill_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILE_NAME);

        // Boot at v3 with a stored backfill offset and a request row.
        {
            let conn = Connection::open(&path).unwrap();
            configure_connection(&conn).unwrap();
            apply_migrations(&conn, &MIGRATIONS[..3]).unwrap();
            conn.execute(
                "INSERT INTO ingest_state (file_path, byte_offset, updated_at_ms)
                 VALUES ('/projects/a/s.jsonl', 4096, 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO requests (request_id, session_id, timestamp_ms)
                 VALUES ('req_v3', 's', 1)",
                [],
            )
            .unwrap();
        }

        let db = Db::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as u64);

        // Indexes swapped, request data intact, offsets cleared so the next
        // backfill pass re-reads transcripts and heals query_source.
        let indexes = index_names(db.conn());
        assert!(indexes.iter().any(|i| i == "idx_requests_facet_rollup"));
        assert!(indexes.iter().any(|i| i == "idx_requests_session_rollup"));
        assert!(!indexes.iter().any(|i| i == "idx_requests_time_rollup"));
        assert!(!indexes.iter().any(|i| i == "idx_requests_session_id"));
        let requests: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM requests", [], |row| row.get(0))
            .unwrap();
        assert_eq!(requests, 1);
        let offsets: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM ingest_state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(offsets, 0, "v4 must clear stored backfill offsets");
    }

    #[test]
    fn open_in_dir_creates_missing_nested_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        let db = Db::open_in_dir(&nested).unwrap();
        assert!(nested.join(DB_FILE_NAME).exists());
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as u64);
    }

    #[test]
    fn sessions_keyed_on_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_in_dir(dir.path()).unwrap();
        let pk: String = db
            .conn()
            .query_row(
                "SELECT name FROM pragma_table_info('sessions') WHERE pk = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pk, "session_id");
    }

    #[test]
    fn reopen_is_idempotent_and_preserves_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILE_NAME);

        {
            let db = Db::open(&path).unwrap();
            db.conn()
                .execute(
                    "INSERT INTO requests (session_id, timestamp_ms, model, cost_usd,
                         input_tokens, output_tokens)
                     VALUES ('sess-1', 1700000000000, 'claude-opus-4', 0.42, 100, 50)",
                    [],
                )
                .unwrap();
            db.conn()
                .execute(
                    "INSERT INTO sessions (session_id, cwd, first_seen_ms)
                     VALUES ('sess-1', '/tmp/project', 1700000000000)",
                    [],
                )
                .unwrap();
        }

        let db = Db::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as u64);
        assert_eq!(db.journal_mode().unwrap(), "wal");

        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM requests", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let cwd: String = db
            .conn()
            .query_row(
                "SELECT cwd FROM sessions WHERE session_id = 'sess-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cwd, "/tmp/project");
    }

    #[test]
    fn migration_upgrade_applies_only_new_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILE_NAME);
        let v2: &[&str] = &[
            MIGRATIONS[0],
            "ALTER TABLE requests ADD COLUMN upgrade_marker TEXT;",
        ];

        // Boot at v1 and write a row.
        {
            let conn = Connection::open(&path).unwrap();
            configure_connection(&conn).unwrap();
            apply_migrations(&conn, &MIGRATIONS[..1]).unwrap();
            assert_eq!(schema_version(&conn).unwrap(), 1);
            conn.execute(
                "INSERT INTO requests (session_id, timestamp_ms) VALUES ('old', 1)",
                [],
            )
            .unwrap();
        }

        // Upgrade to v2: version bumps, new column exists, old data intact.
        let conn = Connection::open(&path).unwrap();
        configure_connection(&conn).unwrap();
        apply_migrations(&conn, v2).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 2);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM requests WHERE session_id = 'old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let has_column: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('requests')
                 WHERE name = 'upgrade_marker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_column, 1);

        // Re-running the same migration list is a no-op.
        apply_migrations(&conn, v2).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 2);
    }

    #[test]
    fn v2_collapses_duplicate_request_ids_and_enforces_uniqueness() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILE_NAME);

        // Boot at v1 and create the duplicates v1 allowed: the same
        // request_id twice (OTLP re-delivery) plus two NULL-id rows.
        {
            let conn = Connection::open(&path).unwrap();
            configure_connection(&conn).unwrap();
            apply_migrations(&conn, &MIGRATIONS[..1]).unwrap();
            for (request_id, ts) in [
                (Some("req_dup"), 1),
                (Some("req_dup"), 2),
                (Some("req_other"), 3),
                (None, 4),
                (None, 5),
            ] {
                conn.execute(
                    "INSERT INTO requests (request_id, session_id, timestamp_ms)
                     VALUES (?1, 's', ?2)",
                    rusqlite::params![request_id, ts],
                )
                .unwrap();
            }
        }

        let db = Db::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as u64);

        // The earliest req_dup row survived; NULL ids are untouched.
        let kept: Vec<i64> = {
            let mut stmt = db
                .conn()
                .prepare("SELECT timestamp_ms FROM requests ORDER BY timestamp_ms")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(kept, vec![1, 3, 4, 5]);

        // Re-inserting a stored request_id now violates the unique index…
        let dup = db.conn().execute(
            "INSERT INTO requests (request_id, session_id, timestamp_ms)
             VALUES ('req_dup', 's', 6)",
            [],
        );
        assert!(dup.is_err(), "duplicate request_id must be rejected");
        // …while NULL request_ids stay unconstrained (api_error rows).
        db.conn()
            .execute(
                "INSERT INTO requests (request_id, session_id, timestamp_ms)
                 VALUES (NULL, 's', 7)",
                [],
            )
            .unwrap();
    }

    #[test]
    fn failed_migration_rolls_back_and_keeps_previous_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILE_NAME);
        let conn = Connection::open(&path).unwrap();
        configure_connection(&conn).unwrap();
        apply_migrations(&conn, &MIGRATIONS[..1]).unwrap();

        let broken: &[&str] = &[
            MIGRATIONS[0],
            "CREATE TABLE ok_table (id INTEGER PRIMARY KEY); THIS IS NOT SQL;",
        ];
        assert!(apply_migrations(&conn, broken).is_err());
        assert_eq!(schema_version(&conn).unwrap(), 1);
        let leaked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'ok_table'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0, "partial migration leaked objects");
    }

    fn write_file(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn legacy_migration_moves_db_and_wal_shm_siblings() {
        let root = tempfile::tempdir().unwrap();
        let old_dir = root.path().join("com.peason.claude-usage-tracker");
        let new_dir = root.path().join("com.peason.farthing");
        std::fs::create_dir_all(&old_dir).unwrap();
        write_file(&old_dir.join("usage.db"), "db");
        write_file(&old_dir.join("usage.db-wal"), "wal");
        write_file(&old_dir.join("usage.db-shm"), "shm");

        let moved = migrate_legacy_data_dir(&old_dir, &new_dir).unwrap();
        assert!(moved);
        for name in ["usage.db", "usage.db-wal", "usage.db-shm"] {
            assert!(new_dir.join(name).exists(), "missing {name} in new dir");
            assert!(!old_dir.join(name).exists(), "{name} left in old dir");
        }
        assert_eq!(
            std::fs::read_to_string(new_dir.join("usage.db")).unwrap(),
            "db"
        );
    }

    #[test]
    fn legacy_migration_moves_db_without_wal_shm() {
        let root = tempfile::tempdir().unwrap();
        let old_dir = root.path().join("old");
        let new_dir = root.path().join("new");
        std::fs::create_dir_all(&old_dir).unwrap();
        write_file(&old_dir.join("usage.db"), "db");

        assert!(migrate_legacy_data_dir(&old_dir, &new_dir).unwrap());
        assert!(new_dir.join("usage.db").exists());
        assert!(!new_dir.join("usage.db-wal").exists());
        assert!(!new_dir.join("usage.db-shm").exists());
    }

    #[test]
    fn legacy_migration_prefers_existing_new_db_and_leaves_old_untouched() {
        let root = tempfile::tempdir().unwrap();
        let old_dir = root.path().join("old");
        let new_dir = root.path().join("new");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        write_file(&old_dir.join("usage.db"), "old");
        write_file(&old_dir.join("usage.db-wal"), "old-wal");
        write_file(&new_dir.join("usage.db"), "new");

        let moved = migrate_legacy_data_dir(&old_dir, &new_dir).unwrap();
        assert!(!moved);
        assert_eq!(
            std::fs::read_to_string(new_dir.join("usage.db")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(old_dir.join("usage.db")).unwrap(),
            "old"
        );
        assert!(old_dir.join("usage.db-wal").exists());
    }

    #[test]
    fn legacy_migration_is_a_noop_without_an_old_db() {
        let root = tempfile::tempdir().unwrap();
        let old_dir = root.path().join("missing-old");
        let new_dir = root.path().join("new");

        assert!(!migrate_legacy_data_dir(&old_dir, &new_dir).unwrap());
        // No-op must not even create the new directory.
        assert!(!new_dir.exists());
    }

    #[test]
    fn legacy_migration_then_open_serves_the_migrated_data() {
        let root = tempfile::tempdir().unwrap();
        let old_dir = root.path().join("old");
        let new_dir = root.path().join("new");

        // Build a real database in the old location and close it.
        {
            let db = Db::open_in_dir(&old_dir).unwrap();
            db.conn()
                .execute(
                    "INSERT INTO requests (request_id, session_id, timestamp_ms)
                     VALUES ('req_legacy', 's', 1)",
                    [],
                )
                .unwrap();
        }

        assert!(migrate_legacy_data_dir(&old_dir, &new_dir).unwrap());
        let db = Db::open_in_dir(&new_dir).unwrap();
        let count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM requests WHERE request_id = 'req_legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert!(!old_dir.join(DB_FILE_NAME).exists());
    }

    #[test]
    fn future_schema_version_refuses_to_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILE_NAME);
        {
            let db = Db::open(&path).unwrap();
            db.conn()
                .execute(
                    "UPDATE meta SET value = '999' WHERE key = 'schema_version'",
                    [],
                )
                .unwrap();
        }
        match Db::open(&path) {
            Err(DbError::FutureSchema { found, supported }) => {
                assert_eq!(found, 999);
                assert_eq!(supported, MIGRATIONS.len() as u64);
            }
            other => panic!("expected FutureSchema error, got {other:?}"),
        }
    }
}
