//! Report export — zip assembly, atomic write, progress, notification handoff.
//!
//! The `export` command receives the prebuilt `report.html` and aggregated
//! `summary.csv` (the frontend owns PALETTE/formatters, so it renders these),
//! streams the raw `requests.csv` from a dedicated read-only connection
//! ([`db::Db::open_readonly`]), and assembles all three into a single `.zip`.
//!
//! Atomicity (R13): the zip is built at a sibling temp path and `rename`d onto
//! `destination` only on full success; any error removes the temp so no
//! partial `.zip` is ever left behind.
//!
//! Notification ownership lives entirely in the frontend (Rust cannot observe
//! SvelteKit route changes), so the command returns `elapsed_ms` and the
//! frontend decides whether to fire (R12).
//!
//! Stored-XSS contract (for Unit 5, the report generator): `report_html` is
//! frontend-supplied and written verbatim into a file the recipient opens in a
//! browser. We write it as-is (no re-sanitization, to keep PALETTE/formatters
//! frontend-side), so the report template MUST embed a restrictive
//! `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; …">`
//! (no inline/remote script) and MUST NOT emit any `<script>` element. The
//! chart is regenerated as inline SVG with inline styles — no script is needed
//! to render it.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use tauri::{Emitter, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;

use crate::db::{self, DbPath};
use crate::queries::{export_raw_rows_for, Facets, RawExportRow};

/// Progress event name; the frontend banner listens on this (R11).
pub const EXPORT_PROGRESS_EVENT: &str = "export:progress";

/// Emit a progress event at most this often by row count, so a 1M-row stream
/// doesn't flood the webview IPC. See [`PROGRESS_MIN_INTERVAL_MS`] — whichever
/// is coarser gates an emit (decision recorded in the plan: row-count OR time,
/// whichever fires later). 2048 rows keeps emit volume to ~500 events at 1M
/// rows even in the pathological zero-time case.
const PROGRESS_ROW_INTERVAL: u64 = 2048;

/// Time floor between progress emits (ms). Pairs with [`PROGRESS_ROW_INTERVAL`]:
/// an emit only fires once BOTH at least `PROGRESS_ROW_INTERVAL` rows have been
/// written AND `PROGRESS_MIN_INTERVAL_MS` has elapsed since the last emit, so a
/// fast small stream emits rarely and a slow large one emits steadily (~10/s).
const PROGRESS_MIN_INTERVAL_MS: u128 = 100;

/// Returned to the frontend so it owns the notification decision (R12): fire
/// once if `elapsed_ms` exceeds the slow threshold OR the user navigated away.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub elapsed_ms: u64,
    /// Total raw rows actually streamed into `requests.csv` (matches the
    /// progress denominator unless the window changed mid-read — it can't,
    /// the whole export reads one WAL snapshot).
    pub rows_written: u64,
}

/// Progress payload emitted on [`EXPORT_PROGRESS_EVENT`]. `phase` is `writing`
/// for streaming updates and `done` for the single terminal event.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportProgress {
    phase: &'static str,
    rows_written: u64,
    /// `requests + errors` from the loaded summary; the denominator the bar
    /// renders against. Displayed progress is clamped to <=100% frontend-side.
    total_rows: u64,
}

/// Callback the raw-row stream calls once per row with that row's serialized
/// CSV bytes. The implementation writes them into the open `requests.csv` zip
/// entry and ticks progress; an `io::Error` aborts the stream and the export.
type RowSink<'a> = dyn FnMut(&[u8]) -> io::Result<()> + 'a;

/// The three entries the bundle always contains (R5).
const REPORT_HTML_NAME: &str = "report.html";
const SUMMARY_CSV_NAME: &str = "summary.csv";
const REQUESTS_CSV_NAME: &str = "requests.csv";

/// Header row for the raw request CSV (R8). Column order matches
/// [`RawExportRow`].
const REQUESTS_CSV_HEADER: &str = "timestamp_ms,timestamp_iso,model,query_source,event_type,source,cost_usd,input_tokens,output_tokens,cache_read_tokens,cache_creation_tokens,duration_ms,error,session_id,project\n";

/// Validate the save-dialog `destination` before any write (R13). The path
/// comes from the native dialog, but we guard defensively: it must be an
/// absolute path with a `.zip` extension whose parent directory exists. We do
/// not pre-check writability with a probe (TOCTOU + side effects); a write
/// failure is caught and surfaced as a clean abort instead.
fn validate_destination(destination: &Path) -> Result<(), String> {
    if !destination.is_absolute() {
        return Err(format!(
            "export destination must be an absolute path: {}",
            destination.display()
        ));
    }
    match destination.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("zip") => {}
        _ => {
            return Err(format!(
                "export destination must end in .zip: {}",
                destination.display()
            ))
        }
    }
    let parent = destination.parent().ok_or_else(|| {
        format!(
            "export destination has no parent directory: {}",
            destination.display()
        )
    })?;
    if !parent.is_dir() {
        return Err(format!(
            "export destination directory does not exist: {}",
            parent.display()
        ));
    }
    Ok(())
}

/// RFC 4180 field quoting: wrap in double quotes and double any embedded quote
/// when the value contains a comma, quote, CR, or LF; otherwise emit as-is.
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// Render one raw row as a CSV line (terminated with `\n`). `cost_usd` and
/// `duration_ms` emit empty for `None` (unpriced / not recorded); numeric
/// fields are never quoted. `timestamp_iso` is a human-readable companion to
/// the epoch-ms `timestamp_ms` so the artifact reads in a spreadsheet without
/// a formula.
fn requests_csv_line(row: &RawExportRow) -> String {
    let iso = chrono::DateTime::from_timestamp_millis(row.timestamp_ms)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default();
    let cost = row.cost_usd.map(|c| c.to_string()).unwrap_or_default();
    let duration = row.duration_ms.map(|d| d.to_string()).unwrap_or_default();
    let mut line = String::new();
    line.push_str(&row.timestamp_ms.to_string());
    line.push(',');
    line.push_str(&csv_field(&iso));
    line.push(',');
    line.push_str(&csv_field(row.model.as_deref().unwrap_or_default()));
    line.push(',');
    line.push_str(&csv_field(row.query_source.as_deref().unwrap_or_default()));
    line.push(',');
    line.push_str(&csv_field(&row.event_type));
    line.push(',');
    line.push_str(&csv_field(&row.source));
    line.push(',');
    line.push_str(&cost);
    line.push(',');
    line.push_str(&row.input_tokens.to_string());
    line.push(',');
    line.push_str(&row.output_tokens.to_string());
    line.push(',');
    line.push_str(&row.cache_read_tokens.to_string());
    line.push(',');
    line.push_str(&row.cache_creation_tokens.to_string());
    line.push(',');
    line.push_str(&duration);
    line.push(',');
    line.push_str(&csv_field(row.error.as_deref().unwrap_or_default()));
    line.push(',');
    line.push_str(&csv_field(row.session_id.as_deref().unwrap_or_default()));
    line.push(',');
    line.push_str(&csv_field(row.cwd.as_deref().unwrap_or_default()));
    line.push('\n');
    line
}

/// Build the zip at a temp path next to `destination`, stream the raw CSV via
/// `stream_rows`, and atomically `rename` onto `destination` only on full
/// success. Any error removes the temp file so no partial `.zip` survives
/// (R13). `on_progress(rows_written)` is invoked after each raw row so callers
/// can throttle/emit; `total_rows` is informational for the caller.
///
/// `stream_rows` is given a sink callback it must call once per raw row with
/// that row's already-serialized CSV bytes. Separating the stream source from
/// the assembly keeps this function unit-testable: a test injects a closure
/// that errors mid-stream to prove the atomic-write contract without a DB.
fn write_bundle<S, P>(
    destination: &Path,
    report_html: &str,
    summary_csv: &str,
    mut stream_rows: S,
    mut on_progress: P,
) -> Result<u64, String>
where
    S: FnMut(&mut RowSink) -> Result<(), String>,
    P: FnMut(u64),
{
    validate_destination(destination)?;

    let parent = destination
        .parent()
        .expect("validate_destination guarantees a parent");
    // Sibling temp on the same volume so the final rename is atomic (a rename
    // across filesystems is a copy+delete and not atomic). PID-suffixed to
    // avoid colliding with a concurrent export of a different file.
    let file_name = destination
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export.zip".to_string());
    let tmp = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));

    let rows_written = match build_zip(
        &tmp,
        report_html,
        summary_csv,
        &mut stream_rows,
        &mut on_progress,
    ) {
        Ok(rows) => rows,
        Err(err) => {
            // Best-effort cleanup; the temp is hidden + PID-scoped, so a
            // failed removal can't masquerade as the user's export.
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
    };

    if let Err(err) = std::fs::rename(&tmp, destination) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "cannot finalize export at {}: {err}",
            destination.display()
        ));
    }
    Ok(rows_written)
}

/// Assemble the zip at `tmp`. Split out so [`write_bundle`] owns the
/// temp-cleanup-on-error policy in one place.
fn build_zip<S, P>(
    tmp: &Path,
    report_html: &str,
    summary_csv: &str,
    stream_rows: &mut S,
    on_progress: &mut P,
) -> Result<u64, String>
where
    S: FnMut(&mut RowSink) -> Result<(), String>,
    P: FnMut(u64),
{
    let file = std::fs::File::create(tmp)
        .map_err(|err| format!("cannot create export temp file {}: {err}", tmp.display()))?;
    let mut zip = zip::ZipWriter::new(io::BufWriter::new(file));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zip.start_file(REPORT_HTML_NAME, options)
        .map_err(|err| format!("cannot start {REPORT_HTML_NAME}: {err}"))?;
    zip.write_all(report_html.as_bytes())
        .map_err(|err| format!("cannot write {REPORT_HTML_NAME}: {err}"))?;

    zip.start_file(SUMMARY_CSV_NAME, options)
        .map_err(|err| format!("cannot start {SUMMARY_CSV_NAME}: {err}"))?;
    zip.write_all(summary_csv.as_bytes())
        .map_err(|err| format!("cannot write {SUMMARY_CSV_NAME}: {err}"))?;

    zip.start_file(REQUESTS_CSV_NAME, options)
        .map_err(|err| format!("cannot start {REQUESTS_CSV_NAME}: {err}"))?;
    zip.write_all(REQUESTS_CSV_HEADER.as_bytes())
        .map_err(|err| format!("cannot write {REQUESTS_CSV_NAME} header: {err}"))?;

    // Stream raw rows straight into the zip entry — never materialized as a
    // Vec, never held in memory (R11). The sink writes the row's bytes and
    // ticks the progress callback.
    let mut rows_written: u64 = 0;
    let mut sink_err: Option<String> = None;
    {
        let zip_ref = &mut zip;
        let stream_result = stream_rows(&mut |bytes: &[u8]| {
            zip_ref.write_all(bytes)?;
            rows_written += 1;
            on_progress(rows_written);
            Ok(())
        });
        if let Err(err) = stream_result {
            sink_err = Some(err);
        }
    }
    if let Some(err) = sink_err {
        return Err(err);
    }

    let inner = zip
        .finish()
        .map_err(|err| format!("cannot finalize zip: {err}"))?;
    inner
        .into_inner()
        .map_err(|err| format!("cannot flush export: {err}"))?
        .sync_all()
        .map_err(|err| format!("cannot fsync export: {err}"))?;
    Ok(rows_written)
}

/// Resolve the data directory for the read-only export connection and the
/// user's home dir (for path relativization), shared by the command and tests.
fn open_export_db<R: Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<(db::Db, Option<String>), String> {
    let data_dir = app.state::<DbPath>().0.clone();
    let db = db::Db::open_readonly(&data_dir.join(db::DB_FILE_NAME))
        .map_err(|err| format!("cannot open export database: {err}"))?;
    let home = app
        .path()
        .home_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    Ok((db, home))
}

/// Write the report bundle to `destination`, streaming the raw CSV from the
/// read-only export connection and emitting throttled progress (R5/R8/R11/R13).
///
/// `total_rows` is `requests + errors` from the already-loaded summary so the
/// progress bar is determinate from the first event; the frontend clamps
/// displayed progress to <=100%. `exclude_sessionless` is the per-view flag
/// (the sessions view passes `true` so its raw CSV matches the session-rollup
/// row set — R16); every other view passes `false`. The command stays
/// view-agnostic: per-view differences arrive as parameters.
#[tauri::command]
pub fn export<R: Runtime>(
    app: tauri::AppHandle<R>,
    destination: String,
    facets: Facets,
    report_html: String,
    summary_csv: String,
    total_rows: u64,
    exclude_sessionless: bool,
) -> Result<ExportResult, String> {
    let destination = PathBuf::from(destination);
    // Validate before opening the DB so a bad path fails fast and cheap.
    validate_destination(&destination)?;

    let (db, home) = open_export_db(&app)?;
    let now = chrono::Local::now();
    let start = Instant::now();

    // Throttle progress emits: row-count AND time floor (whichever is coarser),
    // so a 1M-row stream emits ~10/s, never per-row.
    let mut last_emit = Instant::now();
    let emit_progress = |rows_written: u64, last_emit: &mut Instant| {
        if rows_written.is_multiple_of(PROGRESS_ROW_INTERVAL)
            && last_emit.elapsed().as_millis() >= PROGRESS_MIN_INTERVAL_MS
        {
            *last_emit = Instant::now();
            let _ = app.emit(
                EXPORT_PROGRESS_EVENT,
                ExportProgress {
                    phase: "writing",
                    rows_written,
                    total_rows,
                },
            );
        }
    };

    let home_ref = home.as_deref();
    let stream_rows = |sink: &mut RowSink| -> Result<(), String> {
        export_raw_rows_for(&db, &facets, exclude_sessionless, home_ref, now, |row| {
            let line = requests_csv_line(row);
            // The streaming reader's callback is `rusqlite::Error`-typed; an IO
            // failure into the zip surfaces as a SqliteFailure carrying the
            // message so the abort path runs.
            sink(line.as_bytes()).map_err(|err| {
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                    Some(format!("cannot write requests.csv row: {err}")),
                )
            })
        })
        .map_err(|err| format!("cannot stream export rows: {err}"))
    };

    let rows_written = write_bundle(
        &destination,
        &report_html,
        &summary_csv,
        stream_rows,
        |rows| {
            emit_progress(rows, &mut last_emit);
        },
    )?;

    let elapsed_ms = start.elapsed().as_millis() as u64;
    // Single terminal event so the banner can flip to "done" without polling.
    let _ = app.emit(
        EXPORT_PROGRESS_EVENT,
        ExportProgress {
            phase: "done",
            rows_written,
            total_rows,
        },
    );
    Ok(ExportResult {
        elapsed_ms,
        rows_written,
    })
}

/// Fire a single desktop notification for a completed export. The *decision* to
/// fire (export was slow OR the user navigated away from the originating view)
/// lives in the frontend — Rust can't observe SvelteKit route changes — so this
/// command only performs the send when the frontend asks (R12). Best-effort:
/// a denied permission or any plugin error is swallowed (the in-app banner is
/// the primary success surface), never surfaced as an export failure.
#[tauri::command]
pub fn notify_export_done<R: Runtime>(app: tauri::AppHandle<R>, title: String, body: String) {
    let _ = app.notification().builder().title(title).body(body).show();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::path::PathBuf;

    use tempfile::TempDir;
    use zip::ZipArchive;

    /// A raw row with sensible defaults; tests override the fields they assert.
    fn row(timestamp_ms: i64) -> RawExportRow {
        RawExportRow {
            timestamp_ms,
            model: Some("sonnet".into()),
            query_source: None,
            event_type: "api_request".into(),
            source: "otel".into(),
            cost_usd: Some(1.25),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 30,
            cache_creation_tokens: 40,
            duration_ms: Some(500),
            error: None,
            session_id: Some("s1".into()),
            cwd: Some("~/proj/alpha".into()),
        }
    }

    /// Stream `rows` into the sink, then return `Ok`. The production path
    /// serializes via [`requests_csv_line`]; tests reuse it for fidelity.
    fn stream_ok(rows: Vec<RawExportRow>) -> impl FnMut(&mut RowSink) -> Result<(), String> {
        move |sink: &mut RowSink| {
            for r in &rows {
                sink(requests_csv_line(r).as_bytes())
                    .map_err(|err| format!("sink error: {err}"))?;
            }
            Ok(())
        }
    }

    fn dest(dir: &TempDir, name: &str) -> PathBuf {
        dir.path().join(name)
    }

    fn read_zip_entry(path: &Path, name: &str) -> String {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let mut entry = archive.by_name(name).unwrap();
        let mut contents = String::new();
        entry.read_to_string(&mut contents).unwrap();
        contents
    }

    fn zip_entry_names(path: &Path) -> Vec<String> {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect()
    }

    // ---- atomic-write contract (written first; see Execution note) ----

    /// A mid-stream failure must leave NO file at `destination` and NO temp
    /// residue: the bundle is built at a temp path and only `rename`d on full
    /// success (R13).
    #[test]
    fn mid_stream_failure_leaves_no_destination_and_no_temp() {
        let dir = TempDir::new().unwrap();
        let destination = dest(&dir, "report.zip");

        // Stream two rows fine, then fail — simulating a read/IO error partway
        // through a large export.
        let mut emitted = 0u64;
        let stream = |sink: &mut RowSink| -> Result<(), String> {
            sink(requests_csv_line(&row(1)).as_bytes()).unwrap();
            sink(requests_csv_line(&row(2)).as_bytes()).unwrap();
            Err("simulated mid-stream read failure".to_string())
        };
        let result = write_bundle(&destination, "<html></html>", "h\n", stream, |rows| {
            emitted = rows;
        });

        assert!(result.is_err(), "mid-stream failure must abort");
        assert!(
            !destination.exists(),
            "no partial .zip may exist at the destination"
        );
        // No temp residue: the temp lives next to the destination, hidden +
        // PID-suffixed.
        let residue: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".report.zip.tmp"))
            .collect();
        assert!(residue.is_empty(), "no temp file may survive: {residue:?}");
        assert_eq!(emitted, 2, "progress ticked for the rows that did stream");
    }

    // ---- happy path ----

    #[test]
    fn happy_path_writes_zip_with_three_entries() {
        let dir = TempDir::new().unwrap();
        let destination = dest(&dir, "farthing-cost.zip");
        let rows = vec![row(100), row(200), row(300)];

        let written = write_bundle(
            &destination,
            "<html><body>report</body></html>",
            "model,cost\nsonnet,1.25\n",
            stream_ok(rows.clone()),
            |_| {},
        )
        .unwrap();

        assert_eq!(written, 3, "rows_written counts every streamed row");
        assert!(destination.exists(), "the zip lands at the destination");

        let names = zip_entry_names(&destination);
        assert_eq!(
            names,
            vec![
                REPORT_HTML_NAME.to_string(),
                SUMMARY_CSV_NAME.to_string(),
                REQUESTS_CSV_NAME.to_string()
            ],
            "the bundle contains exactly the three expected entries (R5)"
        );

        let html = read_zip_entry(&destination, REPORT_HTML_NAME);
        assert_eq!(html, "<html><body>report</body></html>");

        let requests = read_zip_entry(&destination, REQUESTS_CSV_NAME);
        let lines: Vec<&str> = requests.lines().collect();
        assert_eq!(lines[0], REQUESTS_CSV_HEADER.trim_end());
        assert_eq!(
            lines.len(),
            4,
            "header + one line per streamed row (no Vec materialization)"
        );
    }

    #[test]
    fn zero_row_window_produces_header_only_requests_csv() {
        let dir = TempDir::new().unwrap();
        let destination = dest(&dir, "empty.zip");

        let written = write_bundle(
            &destination,
            "<html></html>",
            "h\n",
            stream_ok(vec![]),
            |_| {},
        )
        .unwrap();

        assert_eq!(written, 0);
        let requests = read_zip_entry(&destination, REQUESTS_CSV_NAME);
        assert_eq!(
            requests, REQUESTS_CSV_HEADER,
            "a zero-row window yields a valid header-only requests.csv (no panic)"
        );
        // The zip is still valid and complete.
        assert_eq!(zip_entry_names(&destination).len(), 3);
    }

    // ---- destination validation (rejected before any write) ----

    #[test]
    fn unwritable_destination_dir_returns_err_with_no_file() {
        let dir = TempDir::new().unwrap();
        // Parent exists and is valid, but make it unwritable so File::create
        // fails; the abort path must clean up and surface an error.
        let subdir = dir.path().join("locked");
        std::fs::create_dir(&subdir).unwrap();
        let mut perms = std::fs::metadata(&subdir).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o500); // r-x: no write
        }
        std::fs::set_permissions(&subdir, perms).unwrap();
        let destination = subdir.join("out.zip");

        let result = write_bundle(
            &destination,
            "<html></html>",
            "h\n",
            stream_ok(vec![row(1)]),
            |_| {},
        );

        // Restore perms so TempDir can clean up.
        let mut restore = std::fs::metadata(&subdir).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            restore.set_mode(0o700);
        }
        std::fs::set_permissions(&subdir, restore).unwrap();

        assert!(result.is_err(), "an unwritable directory must error");
        assert!(!destination.exists(), "no partial file at the destination");
    }

    #[test]
    fn missing_parent_dir_rejected_before_write() {
        let dir = TempDir::new().unwrap();
        let destination = dir.path().join("nope").join("out.zip");
        let err = write_bundle(
            &destination,
            "<html></html>",
            "h\n",
            stream_ok(vec![row(1)]),
            |_| {},
        )
        .unwrap_err();
        assert!(
            err.contains("does not exist"),
            "actionable error names the missing directory: {err}"
        );
        assert!(!destination.exists());
    }

    #[test]
    fn non_zip_extension_rejected() {
        let dir = TempDir::new().unwrap();
        let destination = dest(&dir, "report.tar");
        let err = validate_destination(&destination).unwrap_err();
        assert!(
            err.contains(".zip"),
            "error names the required extension: {err}"
        );
    }

    #[test]
    fn relative_destination_rejected() {
        let destination = PathBuf::from("report.zip");
        let err = validate_destination(&destination).unwrap_err();
        assert!(
            err.contains("absolute"),
            "error names the constraint: {err}"
        );
    }

    #[test]
    fn zip_extension_is_case_insensitive() {
        let dir = TempDir::new().unwrap();
        let destination = dest(&dir, "report.ZIP");
        assert!(validate_destination(&destination).is_ok());
    }

    // ---- progress + CSV serialization ----

    #[test]
    fn progress_callback_fires_per_row_in_order() {
        let dir = TempDir::new().unwrap();
        let destination = dest(&dir, "p.zip");
        let mut ticks = Vec::new();
        write_bundle(
            &destination,
            "<html></html>",
            "h\n",
            stream_ok(vec![row(1), row(2), row(3), row(4)]),
            |rows| ticks.push(rows),
        )
        .unwrap();
        assert_eq!(
            ticks,
            vec![1, 2, 3, 4],
            "one monotonic tick per streamed row"
        );
    }

    #[test]
    fn csv_line_quotes_fields_with_separators_and_quotes() {
        let mut r = row(1700000000000);
        r.error = Some("API Error: 529, \"overloaded\"\nretry".into());
        r.cwd = Some("~/a,b".into());
        let line = requests_csv_line(&r);
        // The error field is quoted and its inner quotes doubled; the embedded
        // newline stays inside the quoted field (one CSV record, two text lines).
        assert!(
            line.contains("\"API Error: 529, \"\"overloaded\"\"\nretry\""),
            "error field is RFC-4180 quoted: {line}"
        );
        assert!(
            line.contains("\"~/a,b\""),
            "cwd with a comma is quoted: {line}"
        );
    }

    #[test]
    fn csv_line_renders_none_cost_and_duration_as_empty() {
        let mut r = row(1);
        r.cost_usd = None;
        r.duration_ms = None;
        r.model = None;
        let line = requests_csv_line(&r);
        // event_type column is the 5th field; ensure empty cost/duration emit
        // as bare empty cells (",,"), not "null".
        assert!(
            !line.contains("null"),
            "None never serializes as the text null"
        );
        // Two consecutive commas appear where empty cells sit.
        assert!(
            line.contains(",,"),
            "empty cells render as adjacent commas: {line}"
        );
    }

    // ---- integration: real Unit 2 stream -> requests.csv reconciles ----

    /// Seed a real `usage.db` and run the actual [`export_raw_rows_for`] stream
    /// through [`write_bundle`], then assert the `requests.csv` row count
    /// equals what Unit 2 returns over the same facets. This wires the command's
    /// real streaming path (Unit 2 -> CSV -> zip) without a mock Tauri runtime,
    /// proving the bundle reflects exactly the underlying rows (R5/R8).
    #[test]
    fn requests_csv_row_count_matches_unit2_over_same_facets() {
        let dir = TempDir::new().unwrap();
        let db = db::Db::open_in_dir(dir.path()).unwrap();
        db.conn()
            .execute(
                "INSERT INTO sessions (session_id, cwd, first_seen_ms)
                 VALUES ('s1', '/home/dev/proj', 1700000000000)",
                [],
            )
            .unwrap();
        // Three api_request rows + one api_error row, all session-bound.
        for ts in [1700000000001i64, 1700000000002, 1700000000003] {
            db.conn()
                .execute(
                    "INSERT INTO requests (session_id, timestamp_ms, model, event_type, cost_usd)
                     VALUES ('s1', ?1, 'sonnet', 'api_request', 0.5)",
                    [ts],
                )
                .unwrap();
        }
        db.conn()
            .execute(
                "INSERT INTO requests (session_id, timestamp_ms, event_type, error)
                 VALUES ('s1', 1700000000004, 'api_error', 'API Error: 529')",
                [],
            )
            .unwrap();

        let facets = Facets::default();
        let now = chrono::Local::now();

        // Independent Unit 2 count over the same facets.
        let mut unit2_count = 0u64;
        export_raw_rows_for(&db, &facets, false, Some("/home/dev"), now, |_| {
            unit2_count += 1;
            Ok(())
        })
        .unwrap();

        // Drive the same stream through the production zip-assembly path.
        let destination = dest(&dir, "out.zip");
        let stream = |sink: &mut RowSink| -> Result<(), String> {
            export_raw_rows_for(&db, &facets, false, Some("/home/dev"), now, |r| {
                sink(requests_csv_line(r).as_bytes()).map_err(|err| {
                    rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                        Some(format!("sink: {err}")),
                    )
                })
            })
            .map_err(|err| format!("stream: {err}"))
        };
        let written = write_bundle(&destination, "<html></html>", "h\n", stream, |_| {}).unwrap();

        assert_eq!(written, unit2_count, "every Unit 2 row is streamed once");
        assert_eq!(unit2_count, 4, "3 api_request + 1 api_error in the window");

        let requests = read_zip_entry(&destination, REQUESTS_CSV_NAME);
        let data_rows = requests.lines().count() - 1; // minus header
        assert_eq!(
            data_rows as u64, unit2_count,
            "requests.csv data rows match the Unit 2 count over the same facets"
        );
        // The api_error row carries its event_type and home-relativized path.
        assert!(
            requests.contains("api_error"),
            "error rows are included (R8)"
        );
        assert!(
            requests.contains("~/proj"),
            "cwd is home-relativized in the CSV (R8)"
        );
    }
}
