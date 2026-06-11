//! Faceted query layer for the desktop analysis views (task 5.2).
//!
//! One shared [`Facets`] struct (date range, project, model, query_source —
//! PRD FR-7) is applied identically across every aggregation command, so the
//! four Epic 5 views can never disagree for the same selection:
//!
//! - [`usage_summary`]: headline totals (cost, four token counts, the 5m/1h
//!   cache-creation split where backfill data provides it, request/error
//!   counts, distinct sessions)
//! - [`usage_series`]: per-local-day buckets of the same totals, optionally
//!   grouped by model or project (the 5.3 stacking toggle)
//! - [`session_rollups`]: per-session rollups with sort/limit pushed into
//!   SQL (the 5.4 table)
//! - [`session_detail`]: one session's drill-in (per-request timeline,
//!   model mix, cache split, source tags — the 5.4 detail panel)
//! - [`project_rollups`]: per-cwd rollups sorted by cost (the 5.6 view)
//! - [`facet_options`]: the distinct project/model lists the facet bar
//!   offers (plus whether an "unknown project" bucket exists)
//!
//! # Facet semantics
//!
//! - **Range**: presets resolve to local-midnight windows via the metrics
//!   helpers (DST-correct); `all` is unbounded; `custom` passes explicit
//!   `[start_ms, end_ms)` unix-ms bounds through.
//! - **Project**: a session's `cwd`. `unknown` selects requests whose
//!   session has no known cwd (NULL cwd or no session row at all) — they
//!   are data, not errors (PRD FR-3/5.4).
//! - **Model**: exact match on `requests.model`.
//! - **Query source**: `subagent` matches `query_source = 'subagent'` (what
//!   backfill writes for transcript sidechain lines); `main` is everything
//!   else, including NULL (rows whose origin was never recorded count as
//!   main rather than disappearing from both filters).
//!
//! Cost is always API-equivalent; rows with unknown pricing contribute
//! tokens but no cost and are surfaced via `unpriced_requests` so the UI
//! can label them visibly instead of silently undercounting (Epic 5
//! acceptance).
//!
//! # Performance shape (Epic 5: <500ms on a 1M-row database)
//!
//! All aggregations are index-only scans over the two v4 covering indexes,
//! verified by the `EXPLAIN QUERY PLAN` tests below:
//!
//! - time-windowed totals ([`usage_summary`], [`usage_series`]) range-scan
//!   the time-leading `idx_requests_facet_rollup`;
//! - per-session grouping ([`session_rollups`], [`project_rollups`], and
//!   the project-grouped series buckets) scans the session-leading
//!   `idx_requests_session_rollup` in index order, so `GROUP BY
//!   session_id` needs no sorter pass (~260ms vs ~590ms at 1M rows).
//!
//! Project facets compile to `session_id IN (SELECT … FROM sessions)`
//! subqueries and cwd display joins happen only against the handful of
//! already-grouped rows; a per-request `LEFT JOIN sessions` measured ~1.9s
//! for a month window at 1M rows, the subquery shape ~60ms.

use serde::{Deserialize, Serialize};
use tauri::{Manager, Runtime};

use crate::db::{Db, DbState};
use crate::metrics;

/// The `requests.query_source` value marking subagent (sidechain) traffic;
/// written by the backfill engine and matched by [`QuerySourceFacet`].
pub const SUBAGENT_QUERY_SOURCE: &str = "subagent";

/// Hard cap on series length: 5 years of daily buckets. An `all` range
/// older than this is truncated to the most recent days.
const MAX_SERIES_DAYS: usize = 1830;

/// Default / maximum row counts for [`session_rollups`].
const DEFAULT_SESSION_LIMIT: u32 = 200;
const MAX_SESSION_LIMIT: u32 = 2000;

// ---------------------------------------------------------------------------
// Facet parameters (deserialized from the frontend)
// ---------------------------------------------------------------------------

/// Date-range facet. Unit presets deserialize from `"day"`, `"week"`,
/// `"month"`, `"all"`; a custom window from
/// `{"custom":{"start_ms":…,"end_ms":…}}` (`[start, end)` unix ms).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeFacet {
    /// The current local calendar day.
    Day,
    /// The trailing 7 local calendar days (today inclusive).
    Week,
    /// The trailing 30 local calendar days (today inclusive).
    Month,
    /// No time bound.
    #[default]
    All,
    Custom {
        start_ms: i64,
        end_ms: i64,
    },
}

impl RangeFacet {
    /// Resolve to `[start, end)` unix-ms bounds; `None` = unbounded. Preset
    /// boundaries are local midnights (DST-correct, same helpers as the
    /// popover metrics so the views reconcile with the menu bar).
    pub fn resolve(self, now: chrono::DateTime<chrono::Local>) -> (Option<i64>, Option<i64>) {
        let trailing = |days: u32| {
            let boundaries = metrics::trailing_day_boundaries(days, now);
            (boundaries.first().copied(), boundaries.last().copied())
        };
        match self {
            RangeFacet::Day => trailing(1),
            RangeFacet::Week => trailing(7),
            RangeFacet::Month => trailing(30),
            RangeFacet::All => (None, None),
            RangeFacet::Custom { start_ms, end_ms } => (Some(start_ms), Some(end_ms)),
        }
    }
}

/// Project facet: `"all"`, `"unknown"` (sessions with no known cwd), or
/// `{"cwd":"/abs/path"}` for one project.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectFacet {
    #[default]
    All,
    /// Requests whose session has a NULL cwd or no session row at all.
    Unknown,
    Cwd(String),
}

/// Request-origin facet (PRD FR-7: main vs subagent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuerySourceFacet {
    #[default]
    All,
    /// Everything not explicitly tagged subagent (including rows whose
    /// origin was never recorded).
    Main,
    Subagent,
}

/// The shared facet parameters every aggregation command accepts. All
/// fields default, so `{}` means "everything".
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Facets {
    pub range: RangeFacet,
    pub project: ProjectFacet,
    /// Exact model name; `None` = all models.
    pub model: Option<String>,
    pub query_source: QuerySourceFacet,
}

/// `INDEXED BY` pins every aggregation to a v4 covering index. Without it
/// the planner's choice depends on whether ANALYZE has ever run: on
/// stat-less databases (every user's) a model facet flips to the
/// non-covering `idx_requests_model`, turning index-only scans into one
/// main-table probe per matching row (~5x slower at 150k rows, worse at
/// 1M). The plans are asserted by the EXPLAIN QUERY PLAN tests below.
///
/// Time-leading: range scans for time-windowed totals.
const FACET_FROM: &str = "FROM requests r INDEXED BY idx_requests_facet_rollup";
/// Session-leading: index-ordered `GROUP BY session_id` (no sorter pass).
const SESSION_FROM: &str = "FROM requests r INDEXED BY idx_requests_session_rollup";

/// SQL fragments for one facet selection: `WHERE` conditions over the
/// `requests r` alias plus their bind parameters, in order. Pure
/// request-table conditions: project facets compile to `session_id IN`
/// subqueries over the tiny `sessions` table, never a per-request join.
struct FacetFilter {
    conditions: Vec<String>,
    params: Vec<rusqlite::types::Value>,
}

impl FacetFilter {
    fn where_clause(&self) -> String {
        if self.conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", self.conditions.join(" AND "))
        }
    }
}

impl Facets {
    /// Build the SQL filter. `include_time` is false for the series query,
    /// which binds its own per-bucket bounds instead.
    fn filter(&self, include_time: bool, now: chrono::DateTime<chrono::Local>) -> FacetFilter {
        let mut filter = FacetFilter {
            conditions: Vec::new(),
            params: Vec::new(),
        };
        if include_time {
            let (start, end) = self.range.resolve(now);
            if let Some(start) = start {
                filter.conditions.push("r.timestamp_ms >= ?".into());
                filter.params.push(start.into());
            }
            if let Some(end) = end {
                filter.conditions.push("r.timestamp_ms < ?".into());
                filter.params.push(end.into());
            }
        }
        if let Some(model) = &self.model {
            filter.conditions.push("r.model = ?".into());
            filter.params.push(model.clone().into());
        }
        match self.query_source {
            QuerySourceFacet::All => {}
            QuerySourceFacet::Main => filter.conditions.push(format!(
                "(r.query_source IS NULL OR r.query_source <> '{SUBAGENT_QUERY_SOURCE}')"
            )),
            QuerySourceFacet::Subagent => filter
                .conditions
                .push(format!("r.query_source = '{SUBAGENT_QUERY_SOURCE}'")),
        }
        match &self.project {
            ProjectFacet::All => {}
            ProjectFacet::Unknown => {
                filter.conditions.push(
                    "(r.session_id IS NULL OR r.session_id NOT IN
                        (SELECT session_id FROM sessions WHERE cwd IS NOT NULL))"
                        .into(),
                );
            }
            ProjectFacet::Cwd(cwd) => {
                filter
                    .conditions
                    .push("r.session_id IN (SELECT session_id FROM sessions WHERE cwd = ?)".into());
                filter.params.push(cwd.clone().into());
            }
        }
        filter
    }
}

// ---------------------------------------------------------------------------
// Shared aggregate SELECT list
// ---------------------------------------------------------------------------

/// The aggregate columns every rollup shares, in this fixed order:
/// cost, input, output, cache_read, cache_creation, requests, unpriced.
const AGG_COLUMNS: &str = "
    COALESCE(SUM(r.cost_usd), 0.0),
    COALESCE(SUM(r.input_tokens), 0),
    COALESCE(SUM(r.output_tokens), 0),
    COALESCE(SUM(r.cache_read_tokens), 0),
    COALESCE(SUM(r.cache_creation_tokens), 0),
    COALESCE(SUM(r.event_type = 'api_request'), 0),
    COALESCE(SUM(r.event_type = 'api_request' AND r.cost_usd IS NULL), 0)";

/// The 5m/1h cache-creation split sums (transcript-exclusive: SUM over
/// all-NULL columns is NULL, so otel-only selections read as "no split").
/// Both covering indexes carry these columns, so the scans stay index-only.
const SPLIT_COLUMNS: &str = "
    SUM(r.cache_creation_5m_tokens),
    SUM(r.cache_creation_1h_tokens)";

/// [`SPLIT_COLUMNS`] with aliases, for the inner per-session stage.
const SPLIT_COLUMNS_ALIASED: &str = "
    SUM(r.cache_creation_5m_tokens) AS cc_5m,
    SUM(r.cache_creation_1h_tokens) AS cc_1h";

/// Re-summing of [`SPLIT_COLUMNS_ALIASED`] in the outer stage (SUM skips
/// NULL inner groups, so an all-NULL selection still reads as NULL).
const SPLIT_COLUMNS_RESUMMED: &str = "SUM(g.cc_5m), SUM(g.cc_1h)";

/// [`AGG_COLUMNS`] with aliases, for use as the inner per-session stage of
/// the two-stage rollups.
const AGG_COLUMNS_ALIASED: &str = "
    COALESCE(SUM(r.cost_usd), 0.0) AS cost,
    COALESCE(SUM(r.input_tokens), 0) AS input,
    COALESCE(SUM(r.output_tokens), 0) AS output,
    COALESCE(SUM(r.cache_read_tokens), 0) AS cache_read,
    COALESCE(SUM(r.cache_creation_tokens), 0) AS cache_creation,
    COALESCE(SUM(r.event_type = 'api_request'), 0) AS requests,
    COALESCE(SUM(r.event_type = 'api_request' AND r.cost_usd IS NULL), 0) AS unpriced";

/// Re-summing of [`AGG_COLUMNS_ALIASED`] in the outer stage (same column
/// order as [`AGG_COLUMNS`]); every outer group has at least one inner row
/// and inner values are never NULL, so no COALESCE is needed.
const AGG_COLUMNS_RESUMMED: &str = "
    SUM(g.cost), SUM(g.input), SUM(g.output),
    SUM(g.cache_read), SUM(g.cache_creation),
    SUM(g.requests), SUM(g.unpriced)";

/// The shared aggregate values, read from a row at `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Aggregates {
    /// API-equivalent cost; unpriced rows contribute nothing.
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    /// `api_request` rows (errors excluded).
    pub requests: i64,
    /// `api_request` rows with unknown model pricing: tokens counted
    /// above, cost excluded.
    pub unpriced_requests: i64,
}

impl Aggregates {
    fn from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            cost_usd: row.get(offset)?,
            input_tokens: row.get(offset + 1)?,
            output_tokens: row.get(offset + 2)?,
            cache_read_tokens: row.get(offset + 3)?,
            cache_creation_tokens: row.get(offset + 4)?,
            requests: row.get(offset + 5)?,
            unpriced_requests: row.get(offset + 6)?,
        })
    }
}

// ---------------------------------------------------------------------------
// usage_summary
// ---------------------------------------------------------------------------

/// Headline totals for one facet selection.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageSummary {
    /// Resolved window (unix ms); `None` = unbounded ("all").
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    #[serde(flatten)]
    pub totals: Aggregates,
    /// 5m/1h cache-creation split; `None` when no matching row carries it
    /// (the split is transcript-exclusive, so otel-only data has none).
    pub cache_creation_5m_tokens: Option<i64>,
    pub cache_creation_1h_tokens: Option<i64>,
    /// `api_error` rows in the window.
    pub errors: i64,
    /// Distinct `session_id`s (resumes never double-count).
    pub sessions: i64,
}

/// Aggregate the summary for one facet selection. Pure DB read; `now`
/// anchors the range presets so tests pin it exactly.
pub fn summary_for(
    db: &Db,
    facets: &Facets,
    now: chrono::DateTime<chrono::Local>,
) -> Result<UsageSummary, rusqlite::Error> {
    let filter = facets.filter(true, now);
    let (start_ms, end_ms) = facets.range.resolve(now);
    let sql = format!(
        "SELECT {AGG_COLUMNS}, {SPLIT_COLUMNS},
            COALESCE(SUM(r.event_type = 'api_error'), 0),
            COUNT(DISTINCT r.session_id)
         {FACET_FROM} {}",
        filter.where_clause(),
    );
    db.conn().query_row(
        &sql,
        rusqlite::params_from_iter(filter.params.iter()),
        |row| {
            Ok(UsageSummary {
                start_ms,
                end_ms,
                totals: Aggregates::from_row(row, 0)?,
                cache_creation_5m_tokens: row.get(7)?,
                cache_creation_1h_tokens: row.get(8)?,
                errors: row.get(9)?,
                sessions: row.get(10)?,
            })
        },
    )
}

/// Frontend query: faceted headline totals.
#[tauri::command]
pub fn usage_summary<R: Runtime>(
    app: tauri::AppHandle<R>,
    facets: Facets,
) -> Result<UsageSummary, String> {
    let state = app.state::<DbState>();
    let db = state.0.lock().expect("db mutex poisoned");
    summary_for(&db, &facets, chrono::Local::now())
        .map_err(|err| format!("cannot query usage summary: {err}"))
}

// ---------------------------------------------------------------------------
// usage_series
// ---------------------------------------------------------------------------

/// Optional series grouping (the 5.3 stacking toggle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesGroupBy {
    #[default]
    None,
    Model,
    Project,
}

/// One bucket (× group key) in the time series.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesPoint {
    /// Bucket opening instant (unix ms, inclusive). Local midnight except
    /// for a custom range's clamped first bucket.
    pub bucket_start_ms: i64,
    /// Group key (model name or project cwd); `None` for ungrouped points
    /// and for the unknown-model/unknown-project bucket.
    pub key: Option<String>,
    #[serde(flatten)]
    pub totals: Aggregates,
    /// 5m/1h cache-creation split; `None` when no matching row carries it
    /// (the split is transcript-exclusive — same semantics as
    /// [`UsageSummary`], so the 5.5 cache chart can stack it per day).
    pub cache_creation_5m_tokens: Option<i64>,
    pub cache_creation_1h_tokens: Option<i64>,
}

/// Local-midnight bucket boundaries covering `[start_ms, end_ms)`, first
/// and last clamped to the window itself so partial-day custom ranges
/// never include rows outside it. At most [`MAX_SERIES_DAYS`] buckets,
/// keeping the most recent days.
fn day_boundaries(start_ms: i64, end_ms: i64) -> Vec<i64> {
    use chrono::TimeZone;
    if start_ms >= end_ms {
        return Vec::new();
    }
    let mut date = chrono::Local
        .timestamp_millis_opt(start_ms)
        .single()
        .expect("instant to local time is unambiguous")
        .date_naive();
    let end_date = chrono::Local
        .timestamp_millis_opt(end_ms - 1)
        .single()
        .expect("instant to local time is unambiguous")
        .date_naive();
    let days = (end_date - date).num_days() as usize + 1;
    if days > MAX_SERIES_DAYS {
        date = end_date - chrono::Duration::days(MAX_SERIES_DAYS as i64 - 1);
    }
    let mut boundaries = vec![metrics::local_midnight_ms(date).max(start_ms)];
    loop {
        date = date.succ_opt().expect("not at the end of the calendar");
        let midnight = metrics::local_midnight_ms(date);
        if midnight >= end_ms {
            boundaries.push(end_ms);
            return boundaries;
        }
        boundaries.push(midnight);
    }
}

/// Aggregate the per-day series for one facet selection. Ungrouped series
/// return one point per bucket (explicit zeros for gap days, same contract
/// as the popover sparkline); grouped series return one point per bucket ×
/// key actually present, ordered by bucket then key.
pub fn series_for(
    db: &Db,
    facets: &Facets,
    group_by: SeriesGroupBy,
    now: chrono::DateTime<chrono::Local>,
) -> Result<Vec<SeriesPoint>, rusqlite::Error> {
    let filter = facets.filter(false, now);

    let (start, end) = facets.range.resolve(now);
    let end_ms = end.unwrap_or_else(|| metrics::local_day_window(now).1);
    let start_ms = match start {
        Some(start) => start,
        // "all": anchor at the oldest matching row.
        None => {
            let sql = format!(
                "SELECT MIN(r.timestamp_ms) {FACET_FROM} {}",
                filter.where_clause(),
            );
            let min: Option<i64> = db.conn().query_row(
                &sql,
                rusqlite::params_from_iter(filter.params.iter()),
                |row| row.get(0),
            )?;
            match min {
                Some(min) => min,
                None => return Ok(Vec::new()),
            }
        }
    };

    let conn = db.conn();
    let mut conditions = vec![
        "r.timestamp_ms >= ?".to_string(),
        "r.timestamp_ms < ?".to_string(),
    ];
    conditions.extend(filter.conditions.iter().cloned());
    let where_clause = format!("WHERE {}", conditions.join(" AND "));

    let mut points = Vec::new();
    let boundaries = day_boundaries(start_ms, end_ms);
    match group_by {
        SeriesGroupBy::None => {
            let sql = format!("SELECT {AGG_COLUMNS}, {SPLIT_COLUMNS} {FACET_FROM} {where_clause}");
            let mut stmt = conn.prepare(&sql)?;
            for window in boundaries.windows(2) {
                let params = bucket_params(window, &filter.params);
                let point = stmt.query_row(rusqlite::params_from_iter(params), |row| {
                    Ok(SeriesPoint {
                        bucket_start_ms: window[0],
                        key: None,
                        totals: Aggregates::from_row(row, 0)?,
                        cache_creation_5m_tokens: row.get(7)?,
                        cache_creation_1h_tokens: row.get(8)?,
                    })
                })?;
                points.push(point);
            }
        }
        SeriesGroupBy::Model => {
            let sql = format!(
                "SELECT r.model, {AGG_COLUMNS}, {SPLIT_COLUMNS} {FACET_FROM} {where_clause}
                 GROUP BY r.model ORDER BY r.model"
            );
            let mut stmt = conn.prepare(&sql)?;
            for window in boundaries.windows(2) {
                let params = bucket_params(window, &filter.params);
                let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
                    Ok(SeriesPoint {
                        bucket_start_ms: window[0],
                        key: row.get(0)?,
                        totals: Aggregates::from_row(row, 1)?,
                        cache_creation_5m_tokens: row.get(8)?,
                        cache_creation_1h_tokens: row.get(9)?,
                    })
                })?;
                for point in rows {
                    points.push(point?);
                }
            }
        }
        SeriesGroupBy::Project => {
            // Two-stage: group the bucket by session on the request index
            // alone, then join the handful of grouped rows to sessions for
            // the cwd key. A per-request join here measured ~60ms per
            // bucket at 1M rows (1.8s for a month of buckets).
            let sql = format!(
                "SELECT s.cwd, {AGG_COLUMNS_RESUMMED}, {SPLIT_COLUMNS_RESUMMED}
                 FROM (
                     SELECT r.session_id AS session_id, {AGG_COLUMNS_ALIASED},
                         {SPLIT_COLUMNS_ALIASED}
                     {FACET_FROM} {where_clause}
                     GROUP BY r.session_id
                 ) g
                 LEFT JOIN sessions s ON s.session_id = g.session_id
                 GROUP BY s.cwd ORDER BY s.cwd"
            );
            let mut stmt = conn.prepare(&sql)?;
            for window in boundaries.windows(2) {
                let params = bucket_params(window, &filter.params);
                let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
                    Ok(SeriesPoint {
                        bucket_start_ms: window[0],
                        key: row.get(0)?,
                        totals: Aggregates::from_row(row, 1)?,
                        cache_creation_5m_tokens: row.get(8)?,
                        cache_creation_1h_tokens: row.get(9)?,
                    })
                })?;
                for point in rows {
                    points.push(point?);
                }
            }
        }
    }
    Ok(points)
}

/// Bind values for one bucket query: the bucket bounds then the facet
/// parameters, matching the condition order built in [`series_for`].
fn bucket_params(
    window: &[i64],
    facet_params: &[rusqlite::types::Value],
) -> Vec<rusqlite::types::Value> {
    let mut params: Vec<rusqlite::types::Value> = vec![window[0].into(), window[1].into()];
    params.extend(facet_params.iter().cloned());
    params
}

/// Frontend query: faceted per-day usage series, optionally grouped.
#[tauri::command]
pub fn usage_series<R: Runtime>(
    app: tauri::AppHandle<R>,
    facets: Facets,
    group_by: Option<SeriesGroupBy>,
) -> Result<Vec<SeriesPoint>, String> {
    let state = app.state::<DbState>();
    let db = state.0.lock().expect("db mutex poisoned");
    series_for(
        &db,
        &facets,
        group_by.unwrap_or_default(),
        chrono::Local::now(),
    )
    .map_err(|err| format!("cannot query usage series: {err}"))
}

// ---------------------------------------------------------------------------
// session_rollups
// ---------------------------------------------------------------------------

/// Sort key for [`session_rollups`] (always combined with descending flag).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSort {
    #[default]
    Cost,
    /// Total of all four token counters.
    Tokens,
    /// `last_ms - first_ms`.
    Duration,
    /// First request timestamp.
    Start,
}

/// One session's rollup.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionRollup {
    pub session_id: String,
    /// Project directory; `None` = unknown project (no cwd mapping).
    pub cwd: Option<String>,
    /// First/last request timestamps inside the facet window (unix ms).
    pub first_ms: i64,
    pub last_ms: i64,
    #[serde(flatten)]
    pub totals: Aggregates,
    /// `api_error` rows in the window.
    pub errors: i64,
    /// Distinct models used, sorted.
    pub models: Vec<String>,
}

/// Aggregate per-session rollups for one facet selection. Requests with no
/// `session_id` cannot be a session and are excluded here (they still count
/// in [`summary_for`]).
pub fn session_rollups_for(
    db: &Db,
    facets: &Facets,
    sort: SessionSort,
    descending: bool,
    limit: u32,
    offset: u32,
    now: chrono::DateTime<chrono::Local>,
) -> Result<Vec<SessionRollup>, rusqlite::Error> {
    let mut filter = facets.filter(true, now);
    filter.conditions.push("r.session_id IS NOT NULL".into());

    let order_key = match sort {
        SessionSort::Cost => "g.cost",
        SessionSort::Tokens => "(g.input + g.output + g.cache_read + g.cache_creation)",
        SessionSort::Duration => "(g.last_ms - g.first_ms)",
        SessionSort::Start => "g.first_ms",
    };
    let direction = if descending { "DESC" } else { "ASC" };
    // Two-stage: the inner stage groups by session over the session-leading
    // covering index (index order, no sorter); the outer stage joins
    // sessions only for the already-grouped rows' cwd display.
    let sql = format!(
        "SELECT g.session_id, s.cwd, g.first_ms, g.last_ms,
            g.cost, g.input, g.output, g.cache_read, g.cache_creation,
            g.requests, g.unpriced, g.errors, g.models
         FROM (
             SELECT r.session_id AS session_id,
                 MIN(r.timestamp_ms) AS first_ms,
                 MAX(r.timestamp_ms) AS last_ms,
                 {AGG_COLUMNS_ALIASED},
                 COALESCE(SUM(r.event_type = 'api_error'), 0) AS errors,
                 GROUP_CONCAT(DISTINCT r.model) AS models
             {SESSION_FROM} {}
             GROUP BY r.session_id
         ) g
         LEFT JOIN sessions s ON s.session_id = g.session_id
         ORDER BY {order_key} {direction}, g.session_id
         LIMIT ? OFFSET ?",
        filter.where_clause(),
    );
    let limit = limit.clamp(1, MAX_SESSION_LIMIT);
    let mut params = filter.params;
    params.push(i64::from(limit).into());
    params.push(i64::from(offset).into());

    let mut stmt = db.conn().prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        let models: Option<String> = row.get(12)?;
        let mut models: Vec<String> = models
            .map(|joined| joined.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        models.sort();
        Ok(SessionRollup {
            session_id: row.get(0)?,
            cwd: row.get(1)?,
            first_ms: row.get(2)?,
            last_ms: row.get(3)?,
            totals: Aggregates {
                cost_usd: row.get(4)?,
                input_tokens: row.get(5)?,
                output_tokens: row.get(6)?,
                cache_read_tokens: row.get(7)?,
                cache_creation_tokens: row.get(8)?,
                requests: row.get(9)?,
                unpriced_requests: row.get(10)?,
            },
            errors: row.get(11)?,
            models,
        })
    })?;
    rows.collect()
}

/// Frontend query: faceted per-session rollups, sorted and paged in SQL.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn session_rollups<R: Runtime>(
    app: tauri::AppHandle<R>,
    facets: Facets,
    sort: Option<SessionSort>,
    descending: Option<bool>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Vec<SessionRollup>, String> {
    let state = app.state::<DbState>();
    let db = state.0.lock().expect("db mutex poisoned");
    session_rollups_for(
        &db,
        &facets,
        sort.unwrap_or_default(),
        descending.unwrap_or(true),
        limit.unwrap_or(DEFAULT_SESSION_LIMIT),
        offset.unwrap_or(0),
        chrono::Local::now(),
    )
    .map_err(|err| format!("cannot query session rollups: {err}"))
}

// ---------------------------------------------------------------------------
// session_detail
// ---------------------------------------------------------------------------

/// Cap on timeline rows returned by [`session_detail`]; the per-model mix
/// and `total_rows` always cover every matching row so the drill-in header
/// can say "showing first N of M".
pub const DETAIL_REQUEST_LIMIT: usize = 1000;

/// One request in a session's drill-in timeline (task 5.4).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RequestDetail {
    pub timestamp_ms: i64,
    pub model: Option<String>,
    /// Request origin tag (`subagent`, `user`, `sdk`, …); `None` = never
    /// recorded (displayed as main).
    pub query_source: Option<String>,
    /// `api_request` or `api_error`.
    pub event_type: String,
    /// Data source tag: `otel` (live) or `backfill` (transcript).
    pub source: String,
    /// API-equivalent cost; `None` = unpriced (or an error row).
    pub cost_usd: Option<f64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    /// 5m/1h cache-creation split where backfill data provides it.
    pub cache_creation_5m_tokens: Option<i64>,
    pub cache_creation_1h_tokens: Option<i64>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
}

/// One model's share of a session (the drill-in model mix), aggregated over
/// every matching row regardless of the timeline cap.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelMix {
    /// `None` = rows with no model recorded (typically error rows).
    pub model: Option<String>,
    #[serde(flatten)]
    pub totals: Aggregates,
}

/// A session's drill-in detail. The same facets as the rollup table apply,
/// so the drill-in always reconciles with the row that was clicked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionDetail {
    pub session_id: String,
    /// Project directory; `None` = unknown project (no cwd mapping) — data,
    /// not an error (PRD FR-3).
    pub cwd: Option<String>,
    /// All matching rows (requests + errors) under the facets; the timeline
    /// below is capped, this never is.
    pub total_rows: i64,
    /// Per-request timeline, timestamp ascending, at most the caller's cap.
    pub requests: Vec<RequestDetail>,
    /// Per-model aggregates over all matching rows, cost-descending.
    pub models: Vec<ModelMix>,
}

/// Read one session's drill-in detail under the same facet selection as the
/// rollup table. Pure DB read; an unknown `session_id` yields empty data,
/// never an error.
pub fn session_detail_for(
    db: &Db,
    session_id: &str,
    facets: &Facets,
    limit: usize,
    now: chrono::DateTime<chrono::Local>,
) -> Result<SessionDetail, rusqlite::Error> {
    let mut filter = facets.filter(true, now);
    filter.conditions.push("r.session_id = ?".into());
    filter.params.push(session_id.to_owned().into());
    let where_clause = filter.where_clause();

    let conn = db.conn();
    let cwd: Option<String> = conn
        .query_row(
            "SELECT cwd FROM sessions WHERE session_id = ?",
            [session_id],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?
        .flatten();

    let total_rows: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM requests r {where_clause}"),
        rusqlite::params_from_iter(filter.params.iter()),
        |row| row.get(0),
    )?;

    // Timeline rows need non-indexed columns (error, duration, source), so
    // this is a plain table read; a single session is at most a few
    // thousand rows behind `idx_requests_session_id`.
    let sql = format!(
        "SELECT r.timestamp_ms, r.model, r.query_source, r.event_type,
            r.source, r.cost_usd, r.input_tokens, r.output_tokens,
            r.cache_read_tokens, r.cache_creation_tokens,
            r.cache_creation_5m_tokens, r.cache_creation_1h_tokens,
            r.duration_ms, r.error
         FROM requests r {where_clause}
         ORDER BY r.timestamp_ms, r.id
         LIMIT ?"
    );
    let mut params = filter.params.clone();
    params.push((limit as i64).into());
    let mut stmt = conn.prepare(&sql)?;
    let requests = stmt
        .query_map(rusqlite::params_from_iter(params), |row| {
            Ok(RequestDetail {
                timestamp_ms: row.get(0)?,
                model: row.get(1)?,
                query_source: row.get(2)?,
                event_type: row.get(3)?,
                source: row.get(4)?,
                cost_usd: row.get(5)?,
                input_tokens: row.get(6)?,
                output_tokens: row.get(7)?,
                cache_read_tokens: row.get(8)?,
                cache_creation_tokens: row.get(9)?,
                cache_creation_5m_tokens: row.get(10)?,
                cache_creation_1h_tokens: row.get(11)?,
                duration_ms: row.get(12)?,
                error: row.get(13)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let sql = format!(
        "SELECT r.model, {AGG_COLUMNS}
         FROM requests r {where_clause}
         GROUP BY r.model
         ORDER BY 2 DESC, r.model"
    );
    let mut stmt = conn.prepare(&sql)?;
    let models = stmt
        .query_map(rusqlite::params_from_iter(filter.params.iter()), |row| {
            Ok(ModelMix {
                model: row.get(0)?,
                totals: Aggregates::from_row(row, 1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(SessionDetail {
        session_id: session_id.to_owned(),
        cwd,
        total_rows,
        requests,
        models,
    })
}

/// Frontend query: one session's drill-in detail under the active facets.
#[tauri::command]
pub fn session_detail<R: Runtime>(
    app: tauri::AppHandle<R>,
    session_id: String,
    facets: Facets,
) -> Result<SessionDetail, String> {
    let state = app.state::<DbState>();
    let db = state.0.lock().expect("db mutex poisoned");
    session_detail_for(
        &db,
        &session_id,
        &facets,
        DETAIL_REQUEST_LIMIT,
        chrono::Local::now(),
    )
    .map_err(|err| format!("cannot query session detail: {err}"))
}

// ---------------------------------------------------------------------------
// project_rollups
// ---------------------------------------------------------------------------

/// One project's rollup.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectRollup {
    /// Project directory; `None` groups every request whose session has no
    /// known cwd ("unknown project").
    pub cwd: Option<String>,
    #[serde(flatten)]
    pub totals: Aggregates,
    /// Distinct sessions that touched this project in the window.
    pub sessions: i64,
}

/// Aggregate per-project rollups for one facet selection, cost-descending.
pub fn project_rollups_for(
    db: &Db,
    facets: &Facets,
    now: chrono::DateTime<chrono::Local>,
) -> Result<Vec<ProjectRollup>, rusqlite::Error> {
    let filter = facets.filter(true, now);
    // Two-stage like session_rollups_for: per-session aggregation over the
    // session-leading covering index, then a join + re-group over the
    // handful of grouped rows. `COUNT(g.session_id)` skips the NULL
    // session-id group, matching `COUNT(DISTINCT r.session_id)` semantics.
    let sql = format!(
        "SELECT s.cwd, {AGG_COLUMNS_RESUMMED}, COUNT(g.session_id)
         FROM (
             SELECT r.session_id AS session_id, {AGG_COLUMNS_ALIASED}
             {SESSION_FROM} {}
             GROUP BY r.session_id
         ) g
         LEFT JOIN sessions s ON s.session_id = g.session_id
         GROUP BY s.cwd
         ORDER BY 2 DESC, s.cwd",
        filter.where_clause(),
    );
    let mut stmt = db.conn().prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(filter.params.iter()), |row| {
        Ok(ProjectRollup {
            cwd: row.get(0)?,
            totals: Aggregates::from_row(row, 1)?,
            sessions: row.get(8)?,
        })
    })?;
    rows.collect()
}

/// Frontend query: faceted per-project rollups sorted by cost.
#[tauri::command]
pub fn project_rollups<R: Runtime>(
    app: tauri::AppHandle<R>,
    facets: Facets,
) -> Result<Vec<ProjectRollup>, String> {
    let state = app.state::<DbState>();
    let db = state.0.lock().expect("db mutex poisoned");
    project_rollups_for(&db, &facets, chrono::Local::now())
        .map_err(|err| format!("cannot query project rollups: {err}"))
}

// ---------------------------------------------------------------------------
// facet_options
// ---------------------------------------------------------------------------

/// The option lists the facet bar offers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FacetOptions {
    /// Distinct known project directories, sorted.
    pub projects: Vec<String>,
    /// Whether an "unknown project" bucket exists (sessions without a cwd,
    /// or requests without a session id).
    pub unknown_project: bool,
    /// Distinct model names observed, sorted.
    pub models: Vec<String>,
}

/// Read the global facet option lists (unfaceted: the bar's options never
/// shrink because of the current selection).
pub fn facet_options_for(db: &Db) -> Result<FacetOptions, rusqlite::Error> {
    let conn = db.conn();
    let mut stmt =
        conn.prepare("SELECT DISTINCT cwd FROM sessions WHERE cwd IS NOT NULL ORDER BY cwd")?;
    let projects = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    let unknown_project: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sessions WHERE cwd IS NULL)
             OR EXISTS (SELECT 1 FROM requests WHERE session_id IS NULL)",
        [],
        |row| row.get(0),
    )?;
    let mut stmt =
        conn.prepare("SELECT DISTINCT model FROM requests WHERE model IS NOT NULL ORDER BY model")?;
    let models = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(FacetOptions {
        projects,
        unknown_project,
        models,
    })
}

/// Frontend query: project/model option lists for the facet bar.
#[tauri::command]
pub fn facet_options<R: Runtime>(app: tauri::AppHandle<R>) -> Result<FacetOptions, String> {
    let state = app.state::<DbState>();
    let db = state.0.lock().expect("db mutex poisoned");
    facet_options_for(&db).map_err(|err| format!("cannot query facet options: {err}"))
}

// ---------------------------------------------------------------------------
// home_dir
// ---------------------------------------------------------------------------

/// Frontend query: the user's home directory, so the views can display
/// project paths cleaned (`~/Projects/…`, PRD FR-3). `None` when the home
/// directory can't be resolved; the UI then shows absolute paths unchanged.
#[tauri::command]
pub fn home_dir<R: Runtime>(app: tauri::AppHandle<R>) -> Option<String> {
    app.path()
        .home_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    use rusqlite::params;
    use tempfile::TempDir;

    const DAY_MS: i64 = 86_400_000;
    /// Opening instant of the hand-computed fixture window. Tests that need
    /// real local-midnight alignment use `chrono::Local::now()` instead.
    const T: i64 = 1_781_150_400_000;

    fn test_db() -> (TempDir, Db) {
        let dir = TempDir::new().unwrap();
        let db = Db::open_in_dir(dir.path()).unwrap();
        (dir, db)
    }

    fn insert_session(db: &Db, session_id: &str, cwd: Option<&str>) {
        db.conn()
            .execute(
                "INSERT INTO sessions (session_id, cwd, first_seen_ms)
                 VALUES (?1, ?2, ?3)",
                params![session_id, cwd, T],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_request(
        db: &Db,
        session_id: Option<&str>,
        timestamp_ms: i64,
        model: Option<&str>,
        query_source: Option<&str>,
        cost_usd: Option<f64>,
        tokens: (i64, i64, i64, i64),
        splits: (Option<i64>, Option<i64>),
        event_type: &str,
    ) {
        db.conn()
            .execute(
                "INSERT INTO requests (
                    session_id, timestamp_ms, model, query_source, cost_usd,
                    input_tokens, output_tokens, cache_read_tokens,
                    cache_creation_tokens, cache_creation_5m_tokens,
                    cache_creation_1h_tokens, event_type
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    session_id,
                    timestamp_ms,
                    model,
                    query_source,
                    cost_usd,
                    tokens.0,
                    tokens.1,
                    tokens.2,
                    tokens.3,
                    splits.0,
                    splits.1,
                    event_type
                ],
            )
            .unwrap();
    }

    /// The hand-computed fixture every aggregation test reuses:
    ///
    /// | row | session | day | model  | source     | cost | in  | out | cr | cc | 5m/1h  |
    /// |-----|---------|-----|--------|------------|------|-----|-----|----|----|--------|
    /// | r1  | s1 (alpha) | 0 | sonnet | NULL     | 1.0  | 10  | 20  | 30 | 40 | 30/10  |
    /// | r2  | s1 (alpha) | 0 | opus   | subagent | 2.0  | 1   | 2   | 3  | 4  | -      |
    /// | r3  | s2 (beta)  | 0 | sonnet | user     | NULL | 100 | 0   | 0  | 0  | -      |
    /// | r4  | s3 (NULL cwd) | 2 | haiku | sdk    | 0.5  | 5   | 5   | 5  | 5  | -      |
    /// | r5  | s4 (no session row) | 0 | sonnet | subagent | 4.0 | 7 | 0 | 0 | 0 | -    |
    /// | r6  | s1 (alpha) | 0 | NULL  | NULL      | api_error, all zero      |
    fn fixture_db() -> (TempDir, Db) {
        let (dir, db) = test_db();
        insert_session(&db, "s1", Some("/proj/alpha"));
        insert_session(&db, "s2", Some("/proj/beta"));
        insert_session(&db, "s3", None);
        let z = (None, None);
        #[rustfmt::skip]
        {
            insert_request(&db, Some("s1"), T + 1, Some("sonnet"), None, Some(1.0), (10, 20, 30, 40), (Some(30), Some(10)), "api_request");
            insert_request(&db, Some("s1"), T + 2, Some("opus"), Some("subagent"), Some(2.0), (1, 2, 3, 4), z, "api_request");
            insert_request(&db, Some("s2"), T + 3, Some("sonnet"), Some("user"), None, (100, 0, 0, 0), z, "api_request");
            insert_request(&db, Some("s3"), T + 2 * DAY_MS + 4, Some("haiku"), Some("sdk"), Some(0.5), (5, 5, 5, 5), z, "api_request");
            insert_request(&db, Some("s4"), T + 5, Some("sonnet"), Some("subagent"), Some(4.0), (7, 0, 0, 0), z, "api_request");
            insert_request(&db, Some("s1"), T + 6, None, None, None, (0, 0, 0, 0), z, "api_error");
        };
        (dir, db)
    }

    fn now() -> chrono::DateTime<chrono::Local> {
        chrono::Local::now()
    }

    fn facets(range: RangeFacet) -> Facets {
        Facets {
            range,
            ..Facets::default()
        }
    }

    fn full_window() -> RangeFacet {
        RangeFacet::Custom {
            start_ms: T,
            end_ms: T + 3 * DAY_MS,
        }
    }

    // ---- summary: hand-computed totals per facet ----

    #[test]
    fn summary_unfaceted_matches_hand_computed_totals() {
        let (_dir, db) = fixture_db();
        let summary = summary_for(&db, &Facets::default(), now()).unwrap();
        assert_eq!(summary.start_ms, None);
        assert_eq!(summary.end_ms, None);
        assert_eq!(summary.totals.cost_usd, 7.5);
        assert_eq!(summary.totals.input_tokens, 123);
        assert_eq!(summary.totals.output_tokens, 27);
        assert_eq!(summary.totals.cache_read_tokens, 38);
        assert_eq!(summary.totals.cache_creation_tokens, 49);
        assert_eq!(summary.totals.requests, 5);
        assert_eq!(summary.totals.unpriced_requests, 1);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.sessions, 4);
        assert_eq!(summary.cache_creation_5m_tokens, Some(30));
        assert_eq!(summary.cache_creation_1h_tokens, Some(10));
    }

    #[test]
    fn summary_split_is_none_when_no_row_carries_it() {
        let (_dir, db) = fixture_db();
        // The opus facet matches only r2, which has no split.
        let summary = summary_for(
            &db,
            &Facets {
                model: Some("opus".into()),
                ..Facets::default()
            },
            now(),
        )
        .unwrap();
        assert_eq!(summary.cache_creation_5m_tokens, None);
        assert_eq!(summary.cache_creation_1h_tokens, None);
    }

    #[test]
    fn summary_model_facet() {
        let (_dir, db) = fixture_db();
        let summary = summary_for(
            &db,
            &Facets {
                model: Some("sonnet".into()),
                ..Facets::default()
            },
            now(),
        )
        .unwrap();
        // r1 + r3 + r5
        assert_eq!(summary.totals.cost_usd, 5.0);
        assert_eq!(summary.totals.requests, 3);
        assert_eq!(summary.totals.unpriced_requests, 1);
        assert_eq!(summary.sessions, 3);
    }

    #[test]
    fn summary_query_source_facet_main_includes_null_and_non_subagent() {
        let (_dir, db) = fixture_db();
        let main = summary_for(
            &db,
            &Facets {
                query_source: QuerySourceFacet::Main,
                ..Facets::default()
            },
            now(),
        )
        .unwrap();
        // r1 (NULL) + r3 (user) + r4 (sdk) + r6 (error, NULL)
        assert_eq!(main.totals.cost_usd, 1.5);
        assert_eq!(main.totals.requests, 3);
        assert_eq!(main.errors, 1);

        let subagent = summary_for(
            &db,
            &Facets {
                query_source: QuerySourceFacet::Subagent,
                ..Facets::default()
            },
            now(),
        )
        .unwrap();
        // r2 + r5
        assert_eq!(subagent.totals.cost_usd, 6.0);
        assert_eq!(subagent.totals.requests, 2);

        // main + subagent partition the request space exactly.
        let all = summary_for(&db, &Facets::default(), now()).unwrap();
        assert_eq!(
            main.totals.requests + subagent.totals.requests,
            all.totals.requests
        );
        assert_eq!(
            main.totals.cost_usd + subagent.totals.cost_usd,
            all.totals.cost_usd
        );
    }

    #[test]
    fn summary_project_facet_cwd_and_unknown() {
        let (_dir, db) = fixture_db();
        let alpha = summary_for(
            &db,
            &Facets {
                project: ProjectFacet::Cwd("/proj/alpha".into()),
                ..Facets::default()
            },
            now(),
        )
        .unwrap();
        // r1 + r2 (+ r6 error)
        assert_eq!(alpha.totals.cost_usd, 3.0);
        assert_eq!(alpha.totals.requests, 2);
        assert_eq!(alpha.errors, 1);
        assert_eq!(alpha.sessions, 1);

        // Unknown = NULL cwd (s3) plus no session row at all (s4).
        let unknown = summary_for(
            &db,
            &Facets {
                project: ProjectFacet::Unknown,
                ..Facets::default()
            },
            now(),
        )
        .unwrap();
        assert_eq!(unknown.totals.cost_usd, 4.5);
        assert_eq!(unknown.totals.requests, 2);
        assert_eq!(unknown.sessions, 2);
    }

    #[test]
    fn summary_facets_combine_conjunctively() {
        let (_dir, db) = fixture_db();
        let summary = summary_for(
            &db,
            &Facets {
                range: full_window(),
                model: Some("sonnet".into()),
                query_source: QuerySourceFacet::Subagent,
                project: ProjectFacet::Unknown,
            },
            now(),
        )
        .unwrap();
        // Only r5 matches all four.
        assert_eq!(summary.totals.cost_usd, 4.0);
        assert_eq!(summary.totals.requests, 1);
        assert_eq!(summary.totals.input_tokens, 7);
    }

    #[test]
    fn summary_custom_range_is_inclusive_start_exclusive_end() {
        let (_dir, db) = fixture_db();
        // [T, T + 1 day) excludes r4 on day 2.
        let summary = summary_for(
            &db,
            &facets(RangeFacet::Custom {
                start_ms: T,
                end_ms: T + DAY_MS,
            }),
            now(),
        )
        .unwrap();
        assert_eq!(summary.totals.cost_usd, 7.0);
        assert_eq!(summary.totals.requests, 4);
        assert_eq!(summary.start_ms, Some(T));
        assert_eq!(summary.end_ms, Some(T + DAY_MS));
    }

    #[test]
    fn summary_empty_db_is_all_zeros() {
        let (_dir, db) = test_db();
        let summary = summary_for(&db, &Facets::default(), now()).unwrap();
        assert_eq!(summary.totals.cost_usd, 0.0);
        assert_eq!(summary.totals.requests, 0);
        assert_eq!(summary.sessions, 0);
        assert_eq!(summary.cache_creation_5m_tokens, None);
    }

    // ---- range presets ----

    #[test]
    fn range_presets_resolve_to_local_midnight_windows() {
        let now = now();
        let (day_start, day_end) = metrics::local_day_window(now);
        assert_eq!(
            RangeFacet::Day.resolve(now),
            (Some(day_start), Some(day_end))
        );

        for (preset, days) in [(RangeFacet::Week, 7u32), (RangeFacet::Month, 30)] {
            let boundaries = metrics::trailing_day_boundaries(days, now);
            assert_eq!(
                preset.resolve(now),
                (boundaries.first().copied(), boundaries.last().copied())
            );
        }
        assert_eq!(RangeFacet::All.resolve(now), (None, None));
    }

    #[test]
    fn week_preset_filters_rows_outside_the_trailing_window() {
        let (_dir, db) = test_db();
        let now_ms = now().timestamp_millis();
        let z = (None, None);
        // Inside the window (now) and far outside it (40 days back).
        insert_request(
            &db,
            Some("new"),
            now_ms,
            Some("m"),
            None,
            Some(1.0),
            (1, 0, 0, 0),
            z,
            "api_request",
        );
        insert_request(
            &db,
            Some("old"),
            now_ms - 40 * DAY_MS,
            Some("m"),
            None,
            Some(8.0),
            (1, 0, 0, 0),
            z,
            "api_request",
        );

        let week = summary_for(&db, &facets(RangeFacet::Week), now()).unwrap();
        assert_eq!(week.totals.cost_usd, 1.0);
        let month = summary_for(&db, &facets(RangeFacet::Month), now()).unwrap();
        assert_eq!(month.totals.cost_usd, 1.0);
        let all = summary_for(&db, &facets(RangeFacet::All), now()).unwrap();
        assert_eq!(all.totals.cost_usd, 9.0);
    }

    // ---- series ----

    #[test]
    fn ungrouped_series_buckets_match_per_window_summaries_with_explicit_gaps() {
        let (_dir, db) = fixture_db();
        let series = series_for(&db, &facets(full_window()), SeriesGroupBy::None, now()).unwrap();
        assert_eq!(series.len(), 3, "one bucket per day, gap day included");
        let costs: Vec<f64> = series.iter().map(|p| p.totals.cost_usd).collect();
        assert_eq!(costs, vec![7.0, 0.0, 0.5]);
        assert!(series.iter().all(|p| p.key.is_none()));

        // Every bucket equals the summary for its window: the chart can
        // never disagree with the headline totals.
        for (i, point) in series.iter().enumerate() {
            assert_eq!(point.bucket_start_ms, T + i as i64 * DAY_MS);
            let window = summary_for(
                &db,
                &facets(RangeFacet::Custom {
                    start_ms: point.bucket_start_ms,
                    end_ms: point.bucket_start_ms + DAY_MS,
                }),
                now(),
            )
            .unwrap();
            assert_eq!(point.totals, window.totals);
            assert_eq!(
                point.cache_creation_5m_tokens,
                window.cache_creation_5m_tokens
            );
            assert_eq!(
                point.cache_creation_1h_tokens,
                window.cache_creation_1h_tokens
            );
        }
    }

    #[test]
    fn series_carries_the_cache_creation_split_per_bucket() {
        let (_dir, db) = fixture_db();
        let series = series_for(&db, &facets(full_window()), SeriesGroupBy::None, now()).unwrap();
        // Day 0: only r1 carries the split (30/10). Day 1 has no rows and
        // day 2's only row (r4) has no split: both read None, not zero.
        let splits: Vec<(Option<i64>, Option<i64>)> = series
            .iter()
            .map(|p| (p.cache_creation_5m_tokens, p.cache_creation_1h_tokens))
            .collect();
        assert_eq!(
            splits,
            vec![(Some(30), Some(10)), (None, None), (None, None)]
        );

        // Grouped series carry the split per key: on day 0 only the sonnet
        // points (r1) have it.
        let grouped = series_for(&db, &facets(full_window()), SeriesGroupBy::Model, now()).unwrap();
        let day0: Vec<(Option<&str>, Option<i64>, Option<i64>)> = grouped
            .iter()
            .filter(|p| p.bucket_start_ms == T)
            .map(|p| {
                (
                    p.key.as_deref(),
                    p.cache_creation_5m_tokens,
                    p.cache_creation_1h_tokens,
                )
            })
            .collect();
        assert_eq!(
            day0,
            vec![
                (None, None, None),
                (Some("opus"), None, None),
                (Some("sonnet"), Some(30), Some(10)),
            ]
        );
    }

    #[test]
    fn series_point_serializes_flat_for_frontend() {
        let point = SeriesPoint {
            bucket_start_ms: 42,
            key: Some("sonnet".into()),
            totals: Aggregates {
                cost_usd: 1.5,
                input_tokens: 1,
                output_tokens: 2,
                cache_read_tokens: 3,
                cache_creation_tokens: 4,
                requests: 5,
                unpriced_requests: 0,
            },
            cache_creation_5m_tokens: Some(3),
            cache_creation_1h_tokens: None,
        };
        assert_eq!(
            serde_json::to_value(&point).unwrap(),
            serde_json::json!({
                "bucket_start_ms": 42,
                "key": "sonnet",
                "cost_usd": 1.5,
                "input_tokens": 1,
                "output_tokens": 2,
                "cache_read_tokens": 3,
                "cache_creation_tokens": 4,
                "requests": 5,
                "unpriced_requests": 0,
                "cache_creation_5m_tokens": 3,
                "cache_creation_1h_tokens": null,
            })
        );
    }

    #[test]
    fn grouped_series_partitions_the_ungrouped_totals() {
        let (_dir, db) = fixture_db();
        let all = facets(full_window());
        let flat = series_for(&db, &all, SeriesGroupBy::None, now()).unwrap();
        for group_by in [SeriesGroupBy::Model, SeriesGroupBy::Project] {
            let grouped = series_for(&db, &all, group_by, now()).unwrap();
            let total_cost: f64 = grouped.iter().map(|p| p.totals.cost_usd).sum();
            let total_requests: i64 = grouped.iter().map(|p| p.totals.requests).sum();
            let flat_cost: f64 = flat.iter().map(|p| p.totals.cost_usd).sum();
            let flat_requests: i64 = flat.iter().map(|p| p.totals.requests).sum();
            assert_eq!(total_cost, flat_cost, "{group_by:?} must partition cost");
            assert_eq!(total_requests, flat_requests);
            // Grouped buckets only exist where rows exist.
            assert!(grouped
                .iter()
                .all(|p| p.totals.requests > 0 || p.totals.cost_usd > 0.0 || p.key.is_none()));
        }
    }

    #[test]
    fn series_grouped_by_model_keys_day_zero_correctly() {
        let (_dir, db) = fixture_db();
        let series = series_for(&db, &facets(full_window()), SeriesGroupBy::Model, now()).unwrap();
        let day0: Vec<(Option<&str>, f64, i64)> = series
            .iter()
            .filter(|p| p.bucket_start_ms == T)
            .map(|p| (p.key.as_deref(), p.totals.cost_usd, p.totals.requests))
            .collect();
        // NULL model (the error row) sorts first, then opus, then sonnet.
        assert_eq!(
            day0,
            vec![
                (None, 0.0, 0),
                (Some("opus"), 2.0, 1),
                (Some("sonnet"), 5.0, 3),
            ]
        );
    }

    #[test]
    fn series_grouped_by_project_buckets_unknown_as_null_key() {
        let (_dir, db) = fixture_db();
        let series =
            series_for(&db, &facets(full_window()), SeriesGroupBy::Project, now()).unwrap();
        let day0: Vec<(Option<&str>, f64)> = series
            .iter()
            .filter(|p| p.bucket_start_ms == T)
            .map(|p| (p.key.as_deref(), p.totals.cost_usd))
            .collect();
        assert_eq!(
            day0,
            vec![
                (None, 4.0), // s4: no session row
                (Some("/proj/alpha"), 3.0),
                (Some("/proj/beta"), 0.0),
            ]
        );
    }

    #[test]
    fn series_all_range_anchors_at_oldest_matching_row() {
        let (_dir, db) = test_db();
        let now = now();
        let now_ms = now.timestamp_millis();
        let z = (None, None);
        insert_request(
            &db,
            Some("a"),
            now_ms - 2 * DAY_MS,
            Some("m"),
            None,
            Some(1.0),
            (1, 0, 0, 0),
            z,
            "api_request",
        );
        insert_request(
            &db,
            Some("a"),
            now_ms,
            Some("m"),
            None,
            Some(2.0),
            (1, 0, 0, 0),
            z,
            "api_request",
        );

        let series = series_for(&db, &facets(RangeFacet::All), SeriesGroupBy::None, now).unwrap();
        // Day buckets from the oldest row's day through today: 3 local days.
        assert_eq!(series.len(), 3);
        assert_eq!(series.iter().map(|p| p.totals.cost_usd).sum::<f64>(), 3.0);
        // The anchor is also facet-aware: filtering to a model with no rows
        // yields an empty series, not an error.
        let none = series_for(
            &db,
            &Facets {
                model: Some("absent".into()),
                ..Facets::default()
            },
            SeriesGroupBy::None,
            now,
        )
        .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn series_empty_db_and_inverted_range_yield_empty() {
        let (_dir, db) = test_db();
        let empty = series_for(&db, &facets(RangeFacet::All), SeriesGroupBy::None, now()).unwrap();
        assert!(empty.is_empty());
        let inverted = series_for(
            &db,
            &facets(RangeFacet::Custom {
                start_ms: T + DAY_MS,
                end_ms: T,
            }),
            SeriesGroupBy::None,
            now(),
        )
        .unwrap();
        assert!(inverted.is_empty());
    }

    #[test]
    fn day_boundaries_clamp_partial_days_and_align_interior_to_midnight() {
        use chrono::TimeZone;
        let now = now();
        let (today_start, today_end) = metrics::local_day_window(now);
        // A custom window from mid-yesterday to mid-today.
        let start = today_start - DAY_MS / 2;
        let end = today_start + DAY_MS / 4;
        let boundaries = day_boundaries(start, end);
        assert_eq!(
            boundaries.first(),
            Some(&start),
            "first bucket clamps to range start"
        );
        assert_eq!(
            boundaries.last(),
            Some(&end),
            "last bucket clamps to range end"
        );
        // Interior boundaries are exact local midnights.
        for ms in &boundaries[1..boundaries.len() - 1] {
            let local = chrono::Local.timestamp_millis_opt(*ms).unwrap();
            assert_eq!(local.format("%H:%M:%S%.3f").to_string(), "00:00:00.000");
        }
        // A whole-day window has exact midnight boundaries on both ends.
        assert_eq!(
            day_boundaries(today_start, today_end),
            vec![today_start, today_end]
        );
    }

    #[test]
    fn day_boundaries_cap_keeps_most_recent_days() {
        let now = now();
        let (_, today_end) = metrics::local_day_window(now);
        let ancient = today_end - 4000 * DAY_MS;
        let boundaries = day_boundaries(ancient, today_end);
        assert_eq!(boundaries.len(), MAX_SERIES_DAYS + 1);
        assert_eq!(boundaries.last(), Some(&today_end));
    }

    // ---- session rollups ----

    #[test]
    fn session_rollups_match_hand_computed_values() {
        let (_dir, db) = fixture_db();
        let rollups = session_rollups_for(
            &db,
            &Facets::default(),
            SessionSort::Cost,
            true,
            100,
            0,
            now(),
        )
        .unwrap();
        assert_eq!(rollups.len(), 4);
        // Cost-descending: s5? no — s4 (4.0), s1 (3.0), s3 (0.5), s2 (0.0).
        let order: Vec<(&str, f64)> = rollups
            .iter()
            .map(|r| (r.session_id.as_str(), r.totals.cost_usd))
            .collect();
        assert_eq!(
            order,
            vec![("s4", 4.0), ("s1", 3.0), ("s3", 0.5), ("s2", 0.0)]
        );

        let s1 = &rollups[1];
        assert_eq!(s1.cwd.as_deref(), Some("/proj/alpha"));
        assert_eq!(s1.first_ms, T + 1);
        assert_eq!(s1.last_ms, T + 6);
        assert_eq!(s1.totals.requests, 2);
        assert_eq!(s1.errors, 1);
        assert_eq!(s1.totals.input_tokens, 11);
        assert_eq!(s1.models, vec!["opus".to_string(), "sonnet".to_string()]);

        // s4 has no session row: unknown project, not an error.
        assert_eq!(rollups[0].cwd, None);
        // s2's only request is unpriced.
        assert_eq!(rollups[3].totals.unpriced_requests, 1);
    }

    #[test]
    fn session_rollups_sort_keys_and_pagination() {
        let (_dir, db) = fixture_db();
        let by_start = session_rollups_for(
            &db,
            &Facets::default(),
            SessionSort::Start,
            false,
            100,
            0,
            now(),
        )
        .unwrap();
        let ids: Vec<&str> = by_start.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(ids, vec!["s1", "s2", "s4", "s3"]);

        // s1 spans T+1..T+6; everything else is a single instant.
        let by_duration = session_rollups_for(
            &db,
            &Facets::default(),
            SessionSort::Duration,
            true,
            100,
            0,
            now(),
        )
        .unwrap();
        assert_eq!(by_duration[0].session_id, "s1");

        let by_tokens = session_rollups_for(
            &db,
            &Facets::default(),
            SessionSort::Tokens,
            true,
            100,
            0,
            now(),
        )
        .unwrap();
        assert_eq!(by_tokens[0].session_id, "s1"); // 11+22+33+44 = 110 total

        // limit/offset page through the cost ordering.
        let page = session_rollups_for(
            &db,
            &Facets::default(),
            SessionSort::Cost,
            true,
            2,
            1,
            now(),
        )
        .unwrap();
        let ids: Vec<&str> = page.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(ids, vec!["s1", "s3"]);
    }

    #[test]
    fn session_rollups_apply_facets_to_the_rolled_up_rows() {
        let (_dir, db) = fixture_db();
        let rollups = session_rollups_for(
            &db,
            &Facets {
                model: Some("sonnet".into()),
                ..Facets::default()
            },
            SessionSort::Cost,
            true,
            100,
            0,
            now(),
        )
        .unwrap();
        // s1 keeps only r1 under the model facet.
        let s1 = rollups.iter().find(|r| r.session_id == "s1").unwrap();
        assert_eq!(s1.totals.cost_usd, 1.0);
        assert_eq!(s1.totals.requests, 1);
        assert_eq!(s1.models, vec!["sonnet".to_string()]);
        // s3 (haiku only) is gone entirely.
        assert!(rollups.iter().all(|r| r.session_id != "s3"));
    }

    // ---- session detail ----

    #[test]
    fn session_detail_matches_fixture_rows_in_timestamp_order() {
        let (_dir, db) = fixture_db();
        let detail = session_detail_for(&db, "s1", &Facets::default(), 1000, now()).unwrap();
        assert_eq!(detail.session_id, "s1");
        assert_eq!(detail.cwd.as_deref(), Some("/proj/alpha"));
        assert_eq!(detail.total_rows, 3);
        assert_eq!(detail.requests.len(), 3);

        // r1: priced sonnet row with the 5m/1h split, default otel source.
        let r1 = &detail.requests[0];
        assert_eq!(r1.timestamp_ms, T + 1);
        assert_eq!(r1.model.as_deref(), Some("sonnet"));
        assert_eq!(r1.query_source, None);
        assert_eq!(r1.event_type, "api_request");
        assert_eq!(r1.source, "otel");
        assert_eq!(r1.cost_usd, Some(1.0));
        assert_eq!(
            (
                r1.input_tokens,
                r1.output_tokens,
                r1.cache_read_tokens,
                r1.cache_creation_tokens
            ),
            (10, 20, 30, 40)
        );
        assert_eq!(r1.cache_creation_5m_tokens, Some(30));
        assert_eq!(r1.cache_creation_1h_tokens, Some(10));

        // r2: subagent source tag.
        assert_eq!(detail.requests[1].query_source.as_deref(), Some("subagent"));

        // r6: the error row keeps its event type, cost stays None.
        let r6 = &detail.requests[2];
        assert_eq!(r6.event_type, "api_error");
        assert_eq!(r6.model, None);
        assert_eq!(r6.cost_usd, None);

        // Model mix covers every row, cost-descending, NULL-model last.
        let mix: Vec<(Option<&str>, f64, i64)> = detail
            .models
            .iter()
            .map(|m| (m.model.as_deref(), m.totals.cost_usd, m.totals.requests))
            .collect();
        assert_eq!(
            mix,
            vec![
                (Some("opus"), 2.0, 1),
                (Some("sonnet"), 1.0, 1),
                (None, 0.0, 0),
            ]
        );
    }

    #[test]
    fn session_detail_applies_facets_and_reconciles_with_the_rollup() {
        let (_dir, db) = fixture_db();
        let facets = Facets {
            model: Some("sonnet".into()),
            ..Facets::default()
        };
        let detail = session_detail_for(&db, "s1", &facets, 1000, now()).unwrap();
        assert_eq!(detail.total_rows, 1, "only r1 matches the model facet");
        assert_eq!(detail.requests.len(), 1);
        assert_eq!(detail.requests[0].model.as_deref(), Some("sonnet"));

        // The drill-in reconciles with the rollup row for the same facets.
        let rollups =
            session_rollups_for(&db, &facets, SessionSort::Cost, true, 100, 0, now()).unwrap();
        let s1 = rollups.iter().find(|r| r.session_id == "s1").unwrap();
        let detail_cost: f64 = detail.requests.iter().filter_map(|r| r.cost_usd).sum();
        let mix_cost: f64 = detail.models.iter().map(|m| m.totals.cost_usd).sum();
        assert_eq!(detail_cost, s1.totals.cost_usd);
        assert_eq!(mix_cost, s1.totals.cost_usd);

        let subagent = session_detail_for(
            &db,
            "s1",
            &Facets {
                query_source: QuerySourceFacet::Subagent,
                ..Facets::default()
            },
            1000,
            now(),
        )
        .unwrap();
        assert_eq!(subagent.total_rows, 1);
        assert_eq!(subagent.requests[0].model.as_deref(), Some("opus"));
    }

    #[test]
    fn session_detail_unknown_cwd_and_unknown_session_are_data_not_errors() {
        let (_dir, db) = fixture_db();
        // s3 has a session row with NULL cwd; s4 has no session row at all.
        let s3 = session_detail_for(&db, "s3", &Facets::default(), 1000, now()).unwrap();
        assert_eq!(s3.cwd, None);
        assert_eq!(s3.total_rows, 1);
        let s4 = session_detail_for(&db, "s4", &Facets::default(), 1000, now()).unwrap();
        assert_eq!(s4.cwd, None);
        assert_eq!(s4.total_rows, 1);
        assert_eq!(s4.requests[0].cost_usd, Some(4.0));

        // A session id with no rows anywhere yields empty data.
        let ghost = session_detail_for(&db, "nope", &Facets::default(), 1000, now()).unwrap();
        assert_eq!(ghost.cwd, None);
        assert_eq!(ghost.total_rows, 0);
        assert!(ghost.requests.is_empty());
        assert!(ghost.models.is_empty());
    }

    #[test]
    fn session_detail_caps_the_timeline_but_not_totals_or_mix() {
        let (_dir, db) = fixture_db();
        let detail = session_detail_for(&db, "s1", &Facets::default(), 2, now()).unwrap();
        assert_eq!(detail.requests.len(), 2, "timeline capped at the limit");
        assert_eq!(
            detail.requests[1].timestamp_ms,
            T + 2,
            "cap keeps the earliest rows"
        );
        assert_eq!(detail.total_rows, 3, "count ignores the cap");
        assert_eq!(detail.models.len(), 3, "model mix ignores the cap");
    }

    #[test]
    fn session_detail_serializes_flat_for_frontend() {
        let (_dir, db) = fixture_db();
        let detail = session_detail_for(&db, "s2", &Facets::default(), 1000, now()).unwrap();
        assert_eq!(
            serde_json::to_value(&detail).unwrap(),
            serde_json::json!({
                "session_id": "s2",
                "cwd": "/proj/beta",
                "total_rows": 1,
                "requests": [{
                    "timestamp_ms": T + 3,
                    "model": "sonnet",
                    "query_source": "user",
                    "event_type": "api_request",
                    "source": "otel",
                    "cost_usd": null,
                    "input_tokens": 100,
                    "output_tokens": 0,
                    "cache_read_tokens": 0,
                    "cache_creation_tokens": 0,
                    "cache_creation_5m_tokens": null,
                    "cache_creation_1h_tokens": null,
                    "duration_ms": null,
                    "error": null,
                }],
                "models": [{
                    "model": "sonnet",
                    "cost_usd": 0.0,
                    "input_tokens": 100,
                    "output_tokens": 0,
                    "cache_read_tokens": 0,
                    "cache_creation_tokens": 0,
                    "requests": 1,
                    "unpriced_requests": 1,
                }],
            })
        );
    }

    // ---- project rollups ----

    #[test]
    fn project_rollups_match_hand_computed_values_cost_descending() {
        let (_dir, db) = fixture_db();
        let rollups = project_rollups_for(&db, &Facets::default(), now()).unwrap();
        let order: Vec<(Option<&str>, f64, i64, i64)> = rollups
            .iter()
            .map(|p| {
                (
                    p.cwd.as_deref(),
                    p.totals.cost_usd,
                    p.totals.requests,
                    p.sessions,
                )
            })
            .collect();
        assert_eq!(
            order,
            vec![
                (None, 4.5, 2, 2),                // s3 (NULL cwd) + s4 (no row)
                (Some("/proj/alpha"), 3.0, 2, 1), // r1 + r2 (r6 error excluded from requests)
                (Some("/proj/beta"), 0.0, 1, 1),  // r3, unpriced
            ]
        );
        assert_eq!(rollups[2].totals.unpriced_requests, 1);
    }

    #[test]
    fn project_rollups_reconcile_with_summary_for_each_project_facet() {
        let (_dir, db) = fixture_db();
        let rollups = project_rollups_for(&db, &Facets::default(), now()).unwrap();
        for rollup in &rollups {
            let project = match &rollup.cwd {
                Some(cwd) => ProjectFacet::Cwd(cwd.clone()),
                None => ProjectFacet::Unknown,
            };
            let summary = summary_for(
                &db,
                &Facets {
                    project,
                    ..Facets::default()
                },
                now(),
            )
            .unwrap();
            assert_eq!(rollup.totals, summary.totals, "rollup for {:?}", rollup.cwd);
            assert_eq!(rollup.sessions, summary.sessions);
        }
    }

    // ---- facet options ----

    #[test]
    fn facet_options_list_projects_models_and_unknown_flag() {
        let (_dir, db) = fixture_db();
        let options = facet_options_for(&db).unwrap();
        assert_eq!(options.projects, vec!["/proj/alpha", "/proj/beta"]);
        assert!(options.unknown_project, "s3 has a NULL cwd");
        assert_eq!(options.models, vec!["haiku", "opus", "sonnet"]);
    }

    #[test]
    fn facet_options_empty_db() {
        let (_dir, db) = test_db();
        let options = facet_options_for(&db).unwrap();
        assert!(options.projects.is_empty());
        assert!(options.models.is_empty());
        assert!(!options.unknown_project);
    }

    // ---- serde contracts (the frontend payload shapes) ----

    #[test]
    fn facets_deserialize_from_frontend_shapes() {
        let parsed: Facets = serde_json::from_value(serde_json::json!({
            "range": "week",
            "project": {"cwd": "/proj/alpha"},
            "model": "sonnet",
            "query_source": "subagent",
        }))
        .unwrap();
        assert_eq!(
            parsed,
            Facets {
                range: RangeFacet::Week,
                project: ProjectFacet::Cwd("/proj/alpha".into()),
                model: Some("sonnet".into()),
                query_source: QuerySourceFacet::Subagent,
            }
        );

        let custom: Facets = serde_json::from_value(serde_json::json!({
            "range": {"custom": {"start_ms": 1, "end_ms": 2}},
            "project": "unknown",
        }))
        .unwrap();
        assert_eq!(
            custom.range,
            RangeFacet::Custom {
                start_ms: 1,
                end_ms: 2
            }
        );
        assert_eq!(custom.project, ProjectFacet::Unknown);
        assert_eq!(custom.model, None);
        assert_eq!(custom.query_source, QuerySourceFacet::All);

        // {} = everything (all fields default).
        let empty: Facets = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(empty, Facets::default());
    }

    #[test]
    fn summary_serializes_flat_for_frontend() {
        let summary = UsageSummary {
            start_ms: Some(1),
            end_ms: Some(2),
            totals: Aggregates {
                cost_usd: 1.5,
                input_tokens: 1,
                output_tokens: 2,
                cache_read_tokens: 3,
                cache_creation_tokens: 4,
                requests: 5,
                unpriced_requests: 1,
            },
            cache_creation_5m_tokens: Some(3),
            cache_creation_1h_tokens: None,
            errors: 1,
            sessions: 2,
        };
        assert_eq!(
            serde_json::to_value(&summary).unwrap(),
            serde_json::json!({
                "start_ms": 1,
                "end_ms": 2,
                "cost_usd": 1.5,
                "input_tokens": 1,
                "output_tokens": 2,
                "cache_read_tokens": 3,
                "cache_creation_tokens": 4,
                "requests": 5,
                "unpriced_requests": 1,
                "cache_creation_5m_tokens": 3,
                "cache_creation_1h_tokens": null,
                "errors": 1,
                "sessions": 2,
            })
        );
    }

    // ---- command wiring over a real (mock-runtime) app ----

    #[test]
    fn commands_read_managed_db_and_apply_default_args() {
        use std::sync::{Arc, Mutex};

        let dir = TempDir::new().unwrap();
        let db = Db::open_in_dir(dir.path()).unwrap();
        insert_session(&db, "s", Some("/proj/live"));
        let now_ms = chrono::Local::now().timestamp_millis();
        insert_request(
            &db,
            Some("s"),
            now_ms,
            Some("m"),
            None,
            Some(0.5),
            (1, 2, 3, 4),
            (None, None),
            "api_request",
        );

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        app.manage(DbState(Arc::new(Mutex::new(db))));
        let handle = app.handle().clone();

        let options = facet_options(handle.clone()).unwrap();
        assert_eq!(options.projects, vec!["/proj/live"]);

        let summary = usage_summary(handle.clone(), Facets::default()).unwrap();
        assert_eq!(summary.totals.cost_usd, 0.5);

        let series = usage_series(handle.clone(), facets(RangeFacet::Day), None).unwrap();
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].totals.cost_usd, 0.5);

        let sessions =
            session_rollups(handle.clone(), Facets::default(), None, None, None, None).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "s");

        let detail = session_detail(handle.clone(), "s".into(), Facets::default()).unwrap();
        assert_eq!(detail.cwd.as_deref(), Some("/proj/live"));
        assert_eq!(detail.requests.len(), 1);
        assert_eq!(detail.models.len(), 1);

        let projects = project_rollups(handle.clone(), Facets::default()).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].cwd.as_deref(), Some("/proj/live"));

        // The 5.6 projects view cleans paths with the home dir; an absolute
        // path (never empty, never `~`-relative) is the whole contract.
        let home = home_dir(handle).expect("mock runtime resolves a home dir");
        assert!(home.starts_with('/'), "absolute home dir, got {home}");
    }

    // ---- index usage (Epic 5 perf acceptance) ----

    fn plan_for(db: &Db, sql: &str) -> String {
        let mut stmt = db
            .conn()
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        let rows = stmt
            .query_map([], |row| row.get::<_, String>("detail"))
            .unwrap();
        rows.map(|r| r.unwrap()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn faceted_aggregations_use_the_covering_facet_index() {
        let (_dir, db) = fixture_db();
        let facets = Facets {
            range: full_window(),
            model: Some("sonnet".into()),
            query_source: QuerySourceFacet::Main,
            project: ProjectFacet::All,
        };
        let filter = facets.filter(true, now());
        let sql = format!(
            "SELECT {AGG_COLUMNS}, SUM(r.cache_creation_5m_tokens),
                SUM(r.cache_creation_1h_tokens),
                COALESCE(SUM(r.event_type = 'api_error'), 0),
                COUNT(DISTINCT r.session_id)
             {FACET_FROM} {}",
            filter.where_clause(),
        );
        // Bind placeholders with literals so EXPLAIN can run as-is.
        let sql = sql.replacen('?', "0", 1);
        let sql = sql.replacen('?', "9999999999999", 1);
        let sql = sql.replacen('?', "'sonnet'", 1);
        let plan = plan_for(&db, &sql);
        assert!(
            plan.contains("COVERING INDEX idx_requests_facet_rollup"),
            "summary must be an index-only range scan, got plan:\n{plan}"
        );

        // The project-faceted shape stays an index-only scan over requests
        // plus a one-shot subquery over sessions — never a per-request join.
        let project_facets = Facets {
            project: ProjectFacet::Cwd("/proj/alpha".into()),
            ..facets
        };
        let filter = project_facets.filter(true, now());
        let sql = format!(
            "SELECT {AGG_COLUMNS} {FACET_FROM} {}",
            filter.where_clause()
        );
        let sql = sql.replacen('?', "0", 1);
        let sql = sql.replacen('?', "9999999999999", 1);
        let sql = sql.replacen('?', "'sonnet'", 1);
        let sql = sql.replacen('?', "'/proj/alpha'", 1);
        let plan = plan_for(&db, &sql);
        assert!(
            plan.contains("COVERING INDEX idx_requests_facet_rollup"),
            "project facet must keep the index-only request scan:\n{plan}"
        );
        assert!(
            plan.contains("LIST SUBQUERY"),
            "project facet must be a one-shot IN subquery, not a join:\n{plan}"
        );
    }

    #[test]
    fn session_grouped_rollups_use_the_session_leading_index() {
        let (_dir, db) = fixture_db();
        let filter = facets(full_window()).filter(true, now());
        let sql = format!(
            "SELECT g.session_id, s.cwd, g.cost FROM (
                 SELECT r.session_id AS session_id, {AGG_COLUMNS_ALIASED}
                 {SESSION_FROM} {} GROUP BY r.session_id
             ) g
             LEFT JOIN sessions s ON s.session_id = g.session_id",
            filter.where_clause(),
        );
        let sql = sql.replacen('?', "0", 1);
        let sql = sql.replacen('?', "9999999999999", 1);
        let plan = plan_for(&db, &sql);
        assert!(
            plan.contains("COVERING INDEX idx_requests_session_rollup"),
            "per-session grouping must scan the session-leading index:\n{plan}"
        );
        assert!(
            !plan.contains("TEMP B-TREE FOR GROUP BY"),
            "session grouping must come from index order, not a sorter:\n{plan}"
        );
        assert!(
            plan.contains("SEARCH s USING PRIMARY KEY"),
            "cwd display must join sessions by primary key after grouping:\n{plan}"
        );
    }

    // ---- performance (Epic 5 <500ms budget; 1M-row check lives in the
    //      seed_metrics_db example, this is the CI-sized guard) ----

    #[test]
    fn faceted_queries_under_500ms_with_150k_rows() {
        let (_dir, db) = test_db();
        let now = now();
        let newest = now.timestamp_millis();

        db.conn().execute_batch("BEGIN").unwrap();
        for s in 0..600 {
            db.conn()
                .execute(
                    "INSERT INTO sessions (session_id, cwd, first_seen_ms)
                     VALUES (?1, ?2, ?3)",
                    params![format!("sess-{s}"), format!("/proj/p{}", s % 12), newest],
                )
                .unwrap();
        }
        {
            let mut stmt = db
                .conn()
                .prepare(
                    "INSERT INTO requests (
                        session_id, timestamp_ms, model, query_source, cost_usd,
                        input_tokens, output_tokens, cache_read_tokens,
                        cache_creation_tokens
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 100, 50, 2000, 100)",
                )
                .unwrap();
            for i in 0i64..150_000 {
                stmt.execute(params![
                    format!("sess-{}", i % 600),
                    newest - (i * 17_280), // spread over ~30 days
                    ["sonnet", "opus", "haiku"][(i % 3) as usize],
                    (i % 7 == 0).then_some(SUBAGENT_QUERY_SOURCE),
                    0.01,
                ])
                .unwrap();
            }
        }
        db.conn().execute_batch("COMMIT").unwrap();

        let budget = std::time::Duration::from_millis(500);
        let facets = Facets {
            range: RangeFacet::Month,
            model: Some("sonnet".into()),
            query_source: QuerySourceFacet::Main,
            project: ProjectFacet::Cwd("/proj/p3".into()),
        };
        // Project-only over a month is the historical worst case: every
        // window row used to cost a sessions join probe.
        let project_only = Facets {
            range: RangeFacet::Month,
            project: ProjectFacet::Cwd("/proj/p3".into()),
            ..Facets::default()
        };
        let month = Facets {
            range: RangeFacet::Month,
            ..Facets::default()
        };
        for (name, elapsed) in [
            ("summary", {
                let t = std::time::Instant::now();
                summary_for(&db, &facets, now).unwrap();
                t.elapsed()
            }),
            ("summary (project only)", {
                let t = std::time::Instant::now();
                summary_for(&db, &project_only, now).unwrap();
                t.elapsed()
            }),
            ("series", {
                let t = std::time::Instant::now();
                series_for(&db, &facets, SeriesGroupBy::Model, now).unwrap();
                t.elapsed()
            }),
            ("series (month, grouped by project)", {
                let t = std::time::Instant::now();
                series_for(&db, &month, SeriesGroupBy::Project, now).unwrap();
                t.elapsed()
            }),
            ("sessions", {
                let t = std::time::Instant::now();
                session_rollups_for(&db, &facets, SessionSort::Cost, true, 200, 0, now).unwrap();
                t.elapsed()
            }),
            ("sessions (month, unfaceted)", {
                let t = std::time::Instant::now();
                session_rollups_for(&db, &month, SessionSort::Cost, true, 200, 0, now).unwrap();
                t.elapsed()
            }),
            ("projects", {
                let t = std::time::Instant::now();
                project_rollups_for(&db, &facets, now).unwrap();
                t.elapsed()
            }),
            ("projects (month, unfaceted)", {
                let t = std::time::Instant::now();
                project_rollups_for(&db, &month, now).unwrap();
                t.elapsed()
            }),
            ("unfaceted summary", {
                let t = std::time::Instant::now();
                summary_for(&db, &Facets::default(), now).unwrap();
                t.elapsed()
            }),
        ] {
            assert!(elapsed < budget, "{name} took {elapsed:?} on 150k rows");
        }
    }
}
