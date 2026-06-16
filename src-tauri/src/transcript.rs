//! Transcript JSONL parsing (task 3.2).
//!
//! Claude Code writes a JSONL transcript per session under
//! `~/.claude/projects/<flattened-cwd>/<session-id>.jsonl` (subagent
//! sidechains live in `<session-id>/subagents/**.jsonl` next to it). Each
//! `assistant` line carries `message.usage` — the same token counts the OTel
//! `api_request` event exports, plus a 5m/1h cache-creation split the OTel
//! event lacks. This module turns those lines into [`AssistantUsage`] values
//! for the backfill engine (task 3.4).
//!
//! It also extracts full message content (user + assistant turns) into
//! [`TranscriptMessage`] values, enabling conversation-level features without
//! re-reading the file. Message extraction is best-effort and never touches
//! [`ParseStats`] — missing fields are silently skipped.
//!
//! # Tolerance rules
//!
//! Transcripts are an undocumented, fast-moving format (a 481-file corpus
//! scan found 10+ line types; new ones appear between releases), so parsing
//! is deliberately permissive and never fails a file:
//!
//! - any line whose `type` is not `"assistant"` is skipped silently —
//!   including line types that don't exist yet
//! - lines that are not valid JSON objects are counted in
//!   [`ParseStats::malformed_lines`], never fatal
//! - `assistant` lines missing the required fields (`sessionId`, a parseable
//!   `timestamp`, a `message.usage` object) are counted in
//!   [`ParseStats::invalid_assistant_lines`]
//! - token counts tolerate both JSON numbers and numeric strings; absent
//!   counts default to 0; the `cache_creation` 5m/1h split stays `None` when
//!   the object is missing (older transcript versions)
//!
//! # Streaming line groups (feeds task 3.4)
//!
//! Streaming writes one `assistant` line per content block, so one API
//! request (`requestId`) usually spans several lines. Per the 3.1 dedup
//! decision (`docs/notes/dedup-key.md`), [`collapse_requests`] reduces a
//! parse to one entry per request: the **last line with non-zero usage**
//! wins (covers the cumulative-growth case where `output_tokens` grows
//! between lines), trailing all-zero lines are ignored, and lines with
//! `model == "<synthetic>"` or no `requestId` are dropped entirely (they
//! represent no API traffic).
//!
//! [`collapse_messages`] applies the same last-wins logic to message content:
//! assistant lines collapse per `requestId` (last streaming chunk has fullest
//! content); user lines pass through unchanged.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

/// The model name Claude Code uses for transcript-only placeholder lines
/// ("No response requested."). They carry all-zero usage and represent no
/// API request; backfill must skip them.
pub const SYNTHETIC_MODEL: &str = "<synthetic>";

/// Role of a message turn in the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
}

/// A single conversation turn (user or assistant) extracted from a transcript
/// line. Used to reconstruct conversation history without a second file pass.
///
/// `content` and `tool_use_result` are re-serialized verbatim from the
/// parsed JSON, so downstream consumers get exactly what Claude Code wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptMessage {
    pub uuid: String,
    pub parent_uuid: Option<String>,
    /// Present on `assistant` lines only (`requestId`).
    pub request_id: Option<String>,
    pub session_id: String,
    pub role: MessageRole,
    /// Line `timestamp` in unix milliseconds.
    pub timestamp_ms: i64,
    pub is_sidechain: bool,
    /// `message.content` re-serialized to JSON (always a JSON array).
    pub content: String,
    /// `toolUseResult` re-serialized to JSON; `None` when absent or null.
    pub tool_use_result: Option<String>,
}

/// Usage extracted from one `assistant` transcript line. Token fields mirror
/// the `requests` columns; `cache_creation_{5m,1h}_tokens` are the
/// transcript-exclusive split that backfill may add onto existing otel rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantUsage {
    /// `requestId` (`req_…`) — the exact dedup key per spike 3.1. `None`
    /// only on synthetic lines.
    pub request_id: Option<String>,
    pub session_id: String,
    /// Line `timestamp` (RFC 3339 UTC) in unix milliseconds.
    pub timestamp_ms: i64,
    pub model: Option<String>,
    /// Absolute project path the session ran in; feeds the session→cwd
    /// self-heal in task 3.4.
    pub cwd: Option<String>,
    /// `true` on subagent (sidechain) lines.
    pub is_sidechain: bool,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    /// `usage.cache_creation.ephemeral_5m_input_tokens`; `None` when the
    /// split object is absent.
    pub cache_creation_5m_tokens: Option<i64>,
    /// `usage.cache_creation.ephemeral_1h_input_tokens`.
    pub cache_creation_1h_tokens: Option<i64>,
}

impl AssistantUsage {
    /// Whether the line reports any token traffic at all. All-zero lines are
    /// either synthetic or the trailing zero-usage lines streaming sometimes
    /// appends after a request's real lines.
    pub fn has_usage(&self) -> bool {
        self.input_tokens != 0
            || self.output_tokens != 0
            || self.cache_read_tokens != 0
            || self.cache_creation_tokens != 0
    }

    fn is_synthetic(&self) -> bool {
        self.model.as_deref() == Some(SYNTHETIC_MODEL)
    }
}

/// Per-parse line accounting, surfaced by the backfill diff report (3.5).
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ParseStats {
    /// Complete lines examined (excludes a trailing unterminated line).
    pub lines_read: u64,
    /// Lines successfully parsed into an [`AssistantUsage`].
    pub assistant_lines: u64,
    /// Non-`assistant` lines (known and unknown types alike).
    pub skipped_lines: u64,
    /// Lines that are not valid JSON objects.
    pub malformed_lines: u64,
    /// `assistant` lines missing required fields (`sessionId`, parseable
    /// `timestamp`, or a `message.usage` object).
    pub invalid_assistant_lines: u64,
}

/// Result of parsing one transcript (or a tail of one).
#[derive(Debug, Default)]
pub struct TranscriptParse {
    /// One entry per parsed `assistant` line, in file order — *not* yet
    /// collapsed per request; see [`collapse_requests`].
    pub lines: Vec<AssistantUsage>,
    pub stats: ParseStats,
    /// Bytes consumed up to and including the last newline-terminated line.
    /// A trailing line without `\n` (possibly mid-write by Claude Code) is
    /// **not** parsed and not counted, so the next incremental pass
    /// (`parse_file_from(path, offset + bytes_consumed)`) re-reads it once
    /// complete. This is the byte-offset contract for task 3.4.
    pub bytes_consumed: u64,
    /// Every `user` and `assistant` line for which message content could be
    /// extracted, in file order — *not* yet collapsed; see
    /// [`collapse_messages`]. Missing required fields → silently absent.
    pub messages: Vec<TranscriptMessage>,
}

/// Parse a whole transcript file from the beginning.
pub fn parse_file(path: &Path) -> io::Result<TranscriptParse> {
    parse_file_from(path, 0)
}

/// Parse a transcript file starting at `offset` (a previous parse's
/// `offset + bytes_consumed`). Seeking past EOF yields an empty parse.
pub fn parse_file_from(path: &Path, offset: u64) -> io::Result<TranscriptParse> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    Ok(parse_reader(BufReader::new(file)))
}

/// Parse transcript JSONL from any reader. Never fails: per-line problems
/// are tallied in [`ParseStats`]; I/O errors end the parse at the bytes
/// already consumed (the next incremental pass picks up from there).
pub fn parse_reader<R: BufRead>(mut reader: R) -> TranscriptParse {
    let mut parse = TranscriptParse::default();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let read = match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if buf.last() != Some(&b'\n') {
            // Unterminated trailing line: possibly mid-write; leave it for
            // the next pass (not parsed, not counted in bytes_consumed).
            break;
        }
        parse.bytes_consumed += read as u64;
        parse.stats.lines_read += 1;
        parse_line(&buf, &mut parse);
    }
    parse
}

fn parse_line(raw: &[u8], parse: &mut TranscriptParse) {
    let stats = &mut parse.stats;
    let Ok(value) = serde_json::from_slice::<Value>(raw) else {
        stats.malformed_lines += 1;
        return;
    };
    if !value.is_object() {
        stats.malformed_lines += 1;
        return;
    }
    let line_type = value.get("type").and_then(Value::as_str);
    match line_type {
        Some("assistant") => {
            match parse_assistant(&value) {
                Some(line) => {
                    stats.assistant_lines += 1;
                    parse.lines.push(line);
                }
                None => stats.invalid_assistant_lines += 1,
            }
            // Best-effort message extraction: never touches stats.
            if let Some(msg) = parse_message(&value, MessageRole::Assistant) {
                parse.messages.push(msg);
            }
        }
        Some("user") => {
            stats.skipped_lines += 1;
            // Best-effort message extraction: never touches stats.
            if let Some(msg) = parse_message(&value, MessageRole::User) {
                parse.messages.push(msg);
            }
        }
        _ => {
            stats.skipped_lines += 1;
        }
    }
}

/// Extract usage from one `assistant` line. `None` when a required field is
/// missing or unparseable.
fn parse_assistant(line: &Value) -> Option<AssistantUsage> {
    let session_id = line.get("sessionId")?.as_str()?.to_owned();
    let timestamp_ms = rfc3339_utc_to_ms(line.get("timestamp")?.as_str()?)?;
    let message = line.get("message")?;
    let usage = message.get("usage")?;
    usage.as_object()?;
    let split = usage.get("cache_creation").filter(|v| v.is_object());

    Some(AssistantUsage {
        request_id: line
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        session_id,
        timestamp_ms,
        model: message
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cwd: line.get("cwd").and_then(Value::as_str).map(str::to_owned),
        is_sidechain: line
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        input_tokens: usage_i64(usage, "input_tokens"),
        output_tokens: usage_i64(usage, "output_tokens"),
        cache_read_tokens: usage_i64(usage, "cache_read_input_tokens"),
        cache_creation_tokens: usage_i64(usage, "cache_creation_input_tokens"),
        cache_creation_5m_tokens: split
            .and_then(|s| s.get("ephemeral_5m_input_tokens"))
            .and_then(value_i64),
        cache_creation_1h_tokens: split
            .and_then(|s| s.get("ephemeral_1h_input_tokens"))
            .and_then(value_i64),
    })
}

/// Extract message content from a `user` or `assistant` transcript line.
/// Returns `None` when any required field (`uuid`, `sessionId`, `timestamp`,
/// `message.content` array) is missing or unparseable. Never touches
/// [`ParseStats`] — all failures are silent.
fn parse_message(value: &Value, role: MessageRole) -> Option<TranscriptMessage> {
    let uuid = value.get("uuid")?.as_str()?.to_owned();
    let session_id = value.get("sessionId")?.as_str()?.to_owned();
    let timestamp_ms = rfc3339_utc_to_ms(value.get("timestamp")?.as_str()?)?;
    let message = value.get("message")?;
    let content_val = message.get("content")?;
    // Require content to be a JSON array.
    content_val.as_array()?;
    let content = serde_json::to_string(content_val).ok()?;

    let parent_uuid = value
        .get("parentUuid")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let request_id = if role == MessageRole::Assistant {
        value
            .get("requestId")
            .and_then(Value::as_str)
            .map(str::to_owned)
    } else {
        None
    };

    let is_sidechain = value
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Re-serialize toolUseResult verbatim; drop if absent or null.
    let tool_use_result = value
        .get("toolUseResult")
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::to_string(v).ok());

    Some(TranscriptMessage {
        uuid,
        parent_uuid,
        request_id,
        session_id,
        role,
        timestamp_ms,
        is_sidechain,
        content,
        tool_use_result,
    })
}

/// A token count: absent or wrong-typed → 0 (counts are additive; absence
/// means "none"), but numeric strings are tolerated like in `ingest.rs`.
fn usage_i64(usage: &Value, key: &str) -> i64 {
    usage.get(key).and_then(value_i64).unwrap_or(0)
}

/// An i64 encoded as either a JSON number or a string.
fn value_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Collapse parsed lines to one entry per API request, applying the 3.1
/// rules (`docs/notes/dedup-key.md`): drop synthetic / `requestId`-less
/// lines, then keep the **last line with non-zero usage** per `requestId`
/// (trailing all-zero lines lose to any earlier real line). Output preserves
/// first-seen request order.
pub fn collapse_requests(lines: &[AssistantUsage]) -> Vec<AssistantUsage> {
    let mut collapsed: Vec<AssistantUsage> = Vec::new();
    for line in lines {
        if line.is_synthetic() {
            continue;
        }
        let Some(request_id) = line.request_id.as_deref() else {
            continue;
        };
        match collapsed
            .iter_mut()
            .find(|c| c.request_id.as_deref() == Some(request_id))
        {
            None => collapsed.push(line.clone()),
            // Later lines win unless they drop back to zero usage after a
            // real line (the trailing-zero streaming artifact).
            Some(existing) => {
                if line.has_usage() || !existing.has_usage() {
                    *existing = line.clone();
                }
            }
        }
    }
    collapsed
}

/// Collapse parsed messages to one entry per conversation turn, applying the
/// same streaming-collapse rules as [`collapse_requests`]:
///
/// - `user` lines pass through unchanged (unique uuids, no collapsing).
/// - `assistant` lines collapse to **one per `request_id`**; the **last**
///   line wins (the last streamed chunk has the fullest cumulative content).
/// - `assistant` lines without a `request_id` are dropped entirely.
///
/// Output preserves first-seen order.
pub fn collapse_messages(messages: &[TranscriptMessage]) -> Vec<TranscriptMessage> {
    let mut collapsed: Vec<TranscriptMessage> = Vec::new();
    for msg in messages {
        match msg.role {
            MessageRole::User => collapsed.push(msg.clone()),
            MessageRole::Assistant => {
                let Some(request_id) = msg.request_id.as_deref() else {
                    // No request_id → drop entirely.
                    continue;
                };
                match collapsed.iter_mut().find(|c| {
                    c.role == MessageRole::Assistant && c.request_id.as_deref() == Some(request_id)
                }) {
                    None => collapsed.push(msg.clone()),
                    Some(existing) => *existing = msg.clone(),
                }
            }
        }
    }
    collapsed
}

/// RFC 3339 UTC timestamp (`2026-06-11T14:50:03.944Z`) → unix milliseconds.
/// Handles the only shape transcripts emit: date, `T`, time, optional
/// fractional seconds, `Z`. Anything else → `None`.
fn rfc3339_utc_to_ms(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (hms, frac) = match time.split_once('.') {
        Some((hms, frac)) => (hms, frac),
        None => (time, ""),
    };
    let mut time_parts = hms.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    // Fractional seconds at any precision, truncated to milliseconds.
    let millis: i64 = if frac.is_empty() {
        0
    } else {
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let padded = format!("{frac:0<3}");
        padded[..3].parse().ok()?
    };

    let days = days_from_civil(year, month, day)?;
    Some((((days * 24 + hour) * 60 + minute) * 60 + second) * 1000 + millis)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`). `None` for a day invalid in that month.
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => return None,
    };
    if day > days_in_month {
        return None;
    }

    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (i64::from(month) + 9) % 12; // March = 0
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    Some(era * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanitized real transcripts — see `tests/fixtures/transcripts/README.md`
    /// for provenance. Structure, ids, timestamps, and usage are verbatim.
    const MAIN_SESSION: &str = include_str!("../tests/fixtures/transcripts/main-session.jsonl");
    const SIDECHAIN: &str = include_str!("../tests/fixtures/transcripts/sidechain.jsonl");
    const EDGE_CASES: &str = include_str!("../tests/fixtures/transcripts/edge-cases.jsonl");
    /// Synthetic fixture for message content extraction tests.
    const CONTENT_BLOCKS: &str = include_str!("../tests/fixtures/transcripts/content-blocks.jsonl");

    fn parse(s: &str) -> TranscriptParse {
        parse_reader(s.as_bytes())
    }

    #[test]
    fn main_session_extracts_every_assistant_field() {
        let parsed = parse(MAIN_SESSION);

        assert_eq!(
            parsed.stats,
            ParseStats {
                lines_read: 25,
                assistant_lines: 6,
                skipped_lines: 19, // last-prompt/mode/permission-mode/attachment/…
                malformed_lines: 0,
                invalid_assistant_lines: 0,
            }
        );
        assert_eq!(parsed.bytes_consumed, MAIN_SESSION.len() as u64);

        // First assistant line of the file, field-for-field.
        assert_eq!(
            parsed.lines[0],
            AssistantUsage {
                request_id: Some("req_011Cbwf9sGnBjoiZz25k4EK8".into()),
                session_id: "5e6aa3df-f340-46ad-8c40-d613f7073b97".into(),
                timestamp_ms: 1_781_189_403_944, // 2026-06-11T14:50:03.944Z
                model: Some("claude-fable-5".into()),
                cwd: Some("/Users/dev/Projects/acme/app".into()),
                is_sidechain: false,
                input_tokens: 17_045,
                output_tokens: 94,
                cache_read_tokens: 23_661,
                cache_creation_tokens: 31_356,
                cache_creation_5m_tokens: Some(0),
                cache_creation_1h_tokens: Some(31_356),
            }
        );
    }

    #[test]
    fn main_session_collapses_streaming_groups_to_one_row_per_request() {
        let parsed = parse(MAIN_SESSION);
        let requests = collapse_requests(&parsed.lines);

        // 6 assistant lines (2 streaming groups) → 2 requests, file order.
        assert_eq!(requests.len(), 2);
        // Group 1: 2 byte-identical-usage lines; the last one wins.
        assert_eq!(
            requests[0].request_id.as_deref(),
            Some("req_011Cbwf9sGnBjoiZz25k4EK8")
        );
        assert_eq!(requests[0].timestamp_ms, 1_781_189_404_478);
        assert_eq!(requests[0].output_tokens, 94);
        // Group 2: 4 lines.
        assert_eq!(
            requests[1].request_id.as_deref(),
            Some("req_011CbwfAFuopq3NdmbdDHmd2")
        );
        assert_eq!(requests[1].timestamp_ms, 1_781_189_413_164);
        assert_eq!(requests[1].output_tokens, 453);
        assert_eq!(requests[1].input_tokens, 2);
        assert_eq!(requests[1].cache_read_tokens, 55_017);
        assert_eq!(requests[1].cache_creation_tokens, 24_494);
        assert_eq!(requests[1].cache_creation_1h_tokens, Some(24_494));
    }

    #[test]
    fn sidechain_lines_carry_is_sidechain_and_the_5m_split() {
        let parsed = parse(SIDECHAIN);

        assert_eq!(parsed.stats.assistant_lines, 4);
        assert!(parsed.lines.iter().all(|l| l.is_sidechain));

        let requests = collapse_requests(&parsed.lines);
        assert_eq!(requests.len(), 2);
        // Streaming grew output 3 → 136 within the group; last line wins.
        assert_eq!(
            requests[0].request_id.as_deref(),
            Some("req_011CbVNWaunYPpxFNfHsJjRh")
        );
        assert_eq!(requests[0].timestamp_ms, 1_779_989_780_096);
        assert_eq!(requests[0].output_tokens, 136);
        assert_eq!(requests[0].cache_creation_tokens, 80_985);
        assert_eq!(requests[0].cache_creation_5m_tokens, Some(80_985));
        assert_eq!(requests[0].cache_creation_1h_tokens, Some(0));
        assert_eq!(
            requests[0].model.as_deref(),
            Some("claude-haiku-4-5-20251001")
        );
        assert_eq!(
            requests[0].cwd.as_deref(),
            Some("/Users/dev/Projects/project2")
        );
    }

    #[test]
    fn edge_cases_malformed_synthetic_and_unknown_lines_are_tolerated() {
        let parsed = parse(EDGE_CASES);

        assert_eq!(
            parsed.stats,
            ParseStats {
                lines_read: 12,
                assistant_lines: 10, // incl. 2 synthetic lines
                skipped_lines: 1,    // the unknown "quantum-checkpoint" type
                malformed_lines: 1,  // truncated JSON line
                invalid_assistant_lines: 0,
            }
        );

        let requests = collapse_requests(&parsed.lines);
        assert_eq!(requests.len(), 2);

        // Cumulative-growth group (the 3.1 corpus exception): output_tokens
        // grew 5 → 1004 across 6 lines of one requestId; last line wins.
        assert_eq!(
            requests[0].request_id.as_deref(),
            Some("req_011CbqsNS9RVtSnLhZqXW4md")
        );
        assert_eq!(requests[0].output_tokens, 1004);
        assert_eq!(requests[0].timestamp_ms, 1_780_925_210_968);

        // Trailing-synthetic group: 2 real lines then a `<synthetic>`
        // all-zero line *carrying the same requestId*; the synthetic line
        // must not clobber the real usage.
        assert_eq!(
            requests[1].request_id.as_deref(),
            Some("req_011CbrD4NZCKqdjx3AeJ9KbQ")
        );
        assert_eq!(requests[1].output_tokens, 6);
        assert_eq!(requests[1].timestamp_ms, 1_780_940_781_834);
        assert_eq!(requests[1].cache_creation_tokens, 370_530);

        // The requestId-less synthetic line is dropped by collapse.
        assert!(parsed
            .lines
            .iter()
            .any(|l| l.request_id.is_none() && !l.has_usage()));
    }

    #[test]
    fn trailing_all_zero_line_loses_to_earlier_real_line() {
        // Distilled from the corpus: same requestId, real usage then a
        // trailing all-zero non-synthetic line.
        let real = AssistantUsage {
            request_id: Some("req_1".into()),
            session_id: "s".into(),
            timestamp_ms: 1,
            model: Some("claude-opus-4-8".into()),
            cwd: None,
            is_sidechain: false,
            input_tokens: 2,
            output_tokens: 6,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cache_creation_5m_tokens: Some(0),
            cache_creation_1h_tokens: Some(0),
        };
        let zero = AssistantUsage {
            timestamp_ms: 2,
            input_tokens: 0,
            output_tokens: 0,
            ..real.clone()
        };
        let collapsed = collapse_requests(&[real.clone(), zero.clone()]);
        assert_eq!(collapsed, vec![real.clone()]);
        // …but an all-zero-only group still yields its last line.
        let collapsed = collapse_requests(std::slice::from_ref(&zero));
        assert_eq!(collapsed, vec![zero]);
    }

    #[test]
    fn unterminated_trailing_line_is_left_for_the_next_pass() {
        let complete = r#"{"type":"mode","mode":"normal"}"#;
        let partial = r#"{"type":"assistant","sessionId":"s","timesta"#;
        let input = format!("{complete}\n{partial}");

        let parsed = parse(&input);
        assert_eq!(parsed.bytes_consumed, (complete.len() + 1) as u64);
        assert_eq!(parsed.stats.lines_read, 1);
        assert_eq!(parsed.stats.malformed_lines, 0); // partial not judged
        assert!(parsed.lines.is_empty());
    }

    #[test]
    fn parse_file_from_resumes_at_a_stored_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");

        // First pass: everything.
        std::fs::write(&path, MAIN_SESSION).unwrap();
        let first = parse_file(&path).unwrap();
        assert_eq!(first.stats.assistant_lines, 6);
        assert_eq!(first.bytes_consumed, MAIN_SESSION.len() as u64);

        // Nothing new → empty parse.
        let again = parse_file_from(&path, first.bytes_consumed).unwrap();
        assert_eq!(again.stats.lines_read, 0);
        assert!(again.lines.is_empty());

        // Claude Code appends two sidechain lines; only they are new.
        let appended: String =
            SIDECHAIN
                .lines()
                .skip(2)
                .take(2)
                .fold(String::new(), |mut acc, l| {
                    acc.push_str(l);
                    acc.push('\n');
                    acc
                });
        std::fs::write(&path, format!("{MAIN_SESSION}{appended}")).unwrap();
        let incremental = parse_file_from(&path, first.bytes_consumed).unwrap();
        assert_eq!(incremental.stats.assistant_lines, 2);
        assert_eq!(incremental.bytes_consumed, appended.len() as u64);

        // Offset past EOF: empty, not an error.
        let past = parse_file_from(&path, 1_000_000_000).unwrap();
        assert_eq!(past.stats.lines_read, 0);
    }

    #[test]
    fn missing_required_fields_count_as_invalid_not_fatal() {
        // No sessionId / no timestamp / no usage / unparseable timestamp.
        let input = concat!(
            r#"{"type":"assistant","timestamp":"2026-06-11T14:50:03.944Z","message":{"usage":{}}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"s","message":{"usage":{}}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"s","timestamp":"2026-06-11T14:50:03.944Z","message":{}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"s","timestamp":"yesterday-ish","message":{"usage":{}}}"#,
            "\n",
            r#"{"type":"assistant","sessionId":"s","timestamp":"2026-06-11T14:50:03.944Z","message":{"usage":{}}}"#,
            "\n",
        );
        let parsed = parse(input);
        assert_eq!(parsed.stats.invalid_assistant_lines, 4);
        assert_eq!(parsed.stats.assistant_lines, 1);
        // The valid-but-empty usage line defaults all counts to 0.
        assert_eq!(parsed.lines[0].input_tokens, 0);
        assert_eq!(parsed.lines[0].cache_creation_5m_tokens, None);
        assert!(!parsed.lines[0].has_usage());
    }

    #[test]
    fn token_counts_tolerate_numeric_strings_and_absent_split() {
        let input = concat!(
            r#"{"type":"assistant","sessionId":"s","timestamp":"2026-06-01T20:06:33.674Z","#,
            r#""message":{"model":"claude-x","usage":{"input_tokens":"7","output_tokens":12}}}"#,
            "\n",
        );
        let parsed = parse(input);
        let line = &parsed.lines[0];
        assert_eq!(line.timestamp_ms, 1_780_344_393_674);
        assert_eq!(line.input_tokens, 7);
        assert_eq!(line.output_tokens, 12);
        assert_eq!(line.cache_read_tokens, 0);
        assert_eq!(line.cache_creation_5m_tokens, None);
        assert_eq!(line.cache_creation_1h_tokens, None);
    }

    #[test]
    fn empty_and_non_object_inputs_never_panic() {
        assert_eq!(parse("").stats, ParseStats::default());
        let parsed = parse("\n42\n\"a string\"\n[1,2]\nnot json at all\n");
        assert_eq!(parsed.stats.malformed_lines, 5);
        assert_eq!(parsed.stats.lines_read, 5);
        assert!(parsed.lines.is_empty());
    }

    // ── Message content extraction tests ─────────────────────────────────────

    #[test]
    fn content_blocks_extracts_messages_without_touching_stats() {
        let parsed = parse(CONTENT_BLOCKS);

        // Existing stats are unchanged.
        assert_eq!(
            parsed.stats,
            ParseStats {
                lines_read: 4,
                assistant_lines: 2,
                skipped_lines: 2, // two user lines
                malformed_lines: 0,
                invalid_assistant_lines: 0,
            }
        );

        // All 4 lines yielded a message.
        assert_eq!(parsed.messages.len(), 4);

        // Line 1: user prompt.
        let u1 = &parsed.messages[0];
        assert_eq!(u1.role, MessageRole::User);
        assert_eq!(u1.uuid, "uuid-u1");
        assert_eq!(u1.parent_uuid, None);
        assert_eq!(u1.request_id, None);
        assert_eq!(u1.session_id, "sess-content");

        // Lines 2 & 3: both assistant streaming chunks for req_content_001.
        assert_eq!(parsed.messages[1].role, MessageRole::Assistant);
        assert_eq!(
            parsed.messages[1].request_id.as_deref(),
            Some("req_content_001")
        );
        assert_eq!(parsed.messages[2].role, MessageRole::Assistant);
        assert_eq!(
            parsed.messages[2].request_id.as_deref(),
            Some("req_content_001")
        );

        // Line 4: tool-result user line with parent pointing at stream2.
        let u2 = &parsed.messages[3];
        assert_eq!(u2.role, MessageRole::User);
        assert_eq!(u2.uuid, "uuid-u2");
        assert_eq!(u2.parent_uuid.as_deref(), Some("uuid-a1-stream2"));
    }

    #[test]
    fn collapse_messages_last_streaming_line_wins() {
        let parsed = parse(CONTENT_BLOCKS);
        let collapsed = collapse_messages(&parsed.messages);

        // 2 user pass-through + 1 collapsed assistant = 3 total.
        assert_eq!(collapsed.len(), 3);

        // First: user prompt (uuid-u1).
        assert_eq!(collapsed[0].uuid, "uuid-u1");
        assert_eq!(collapsed[0].role, MessageRole::User);

        // Second: collapsed assistant — last stream line wins.
        assert_eq!(collapsed[1].uuid, "uuid-a1-stream2");
        assert_eq!(collapsed[1].role, MessageRole::Assistant);
        // The final chunk's content includes the "text" block.
        assert!(
            collapsed[1].content.contains("I'll list them."),
            "collapsed assistant content should contain the fuller stream2 text"
        );

        // Third: tool-result user line (uuid-u2).
        assert_eq!(collapsed[2].uuid, "uuid-u2");
        assert_eq!(collapsed[2].role, MessageRole::User);
    }

    #[test]
    fn collapse_messages_drops_assistant_without_request_id() {
        let msg = TranscriptMessage {
            uuid: "no-req".into(),
            parent_uuid: None,
            request_id: None,
            session_id: "s".into(),
            role: MessageRole::Assistant,
            timestamp_ms: 0,
            is_sidechain: false,
            content: "[]".into(),
            tool_use_result: None,
        };
        let result = collapse_messages(&[msg]);
        assert!(
            result.is_empty(),
            "assistant message without request_id must be dropped"
        );
    }

    #[test]
    fn offset_resume_still_yields_correct_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("content-blocks.jsonl");
        std::fs::write(&path, CONTENT_BLOCKS).unwrap();

        // First full parse.
        let first = parse_file(&path).unwrap();
        assert_eq!(first.messages.len(), 4);

        // Append a new user line.
        let extra = concat!(
            r#"{"type":"user","uuid":"uuid-extra","parentUuid":"uuid-u2","sessionId":"sess-content","timestamp":"2026-06-11T15:00:04.000Z","isSidechain":false,"message":{"role":"user","content":[{"type":"text","text":"done"}]}}"#,
            "\n"
        );
        let mut full = String::from(CONTENT_BLOCKS);
        full.push_str(extra);
        std::fs::write(&path, &full).unwrap();

        // Incremental parse from where the first left off.
        let incremental = parse_file_from(&path, first.bytes_consumed).unwrap();
        assert_eq!(
            incremental.messages.len(),
            1,
            "incremental parse should see only the newly appended line"
        );
        assert_eq!(incremental.messages[0].uuid, "uuid-extra");
    }

    #[test]
    fn rfc3339_utc_parsing() {
        assert_eq!(rfc3339_utc_to_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            rfc3339_utc_to_ms("2026-06-11T14:50:03.944Z"),
            Some(1_781_189_403_944)
        );
        // Leap-year day, short fraction padded to ms.
        assert_eq!(
            rfc3339_utc_to_ms("2000-02-29T12:00:00.5Z"),
            Some(951_825_600_500)
        );
        // Long fraction truncated to ms.
        assert_eq!(
            rfc3339_utc_to_ms("2026-06-11T14:50:03.944999Z"),
            Some(1_781_189_403_944)
        );
        // Pre-epoch dates work (negative ms).
        assert_eq!(rfc3339_utc_to_ms("1969-12-31T23:59:59Z"), Some(-1000));

        for bad in [
            "",
            "2026-06-11",
            "2026-06-11T14:50:03.944", // no Z: transcripts always end in Z
            "2026-06-11T14:50:03+02:00",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z", // not a leap year
            "2026-06-31T00:00:00Z",
            "2026-06-11T24:00:00Z",
            "2026-06-11T14:50:03.x9Z",
            "yesterday-ish",
        ] {
            assert_eq!(rfc3339_utc_to_ms(bad), None, "{bad:?} should not parse");
        }
    }
}
