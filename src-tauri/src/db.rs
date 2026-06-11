//! SQLite persistence layer.
//!
//! Owns the `usage.db` database (WAL mode) and its embedded, versioned
//! migrations. The schema version is tracked in the `meta` table under the
//! `schema_version` key; migrations are idempotent across restarts and only
//! statements newer than the stored version are applied.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::Connection;

/// File name of the database inside the app data directory.
pub const DB_FILE_NAME: &str = "usage.db";

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
/// so shared app state goes through a mutex.
pub struct DbState(pub Mutex<Db>);

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
            "idx_requests_timestamp",
            "idx_requests_session_id",
            "idx_requests_model",
        ] {
            assert!(indexes.iter().any(|i| i == index), "missing index {index}");
        }
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
