//! Session state + line parsing for the codag daemon.
//!
//! A *session* is a long-lived [`StreamingIndex`] keyed by an opaque id. Agents
//! `POST` log lines into it incrementally and `GET` (or `subscribe` to) the
//! projected capsule. State lives entirely in memory; sessions idle longer than
//! [`SESSION_TTL`] are evicted by a background task (see `main.rs`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use codag::{CompressorConfig, LogLine, Mode, StreamingIndex};
use regex::Regex;
use std::sync::OnceLock;
use tokio::sync::RwLock;

/// Sessions idle longer than this are evicted by the background sweeper.
pub const SESSION_TTL: Duration = Duration::from_secs(30 * 60);

/// How often the background sweeper runs.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Cadence of SSE capsule snapshots.
pub const SSE_INTERVAL: Duration = Duration::from_secs(1);

/// One live session: a streaming index plus its last-touch timestamp.
#[derive(Debug)]
pub struct SessionEntry {
    pub index: StreamingIndex,
    pub last_touch: Instant,
}

impl SessionEntry {
    pub fn new() -> Self {
        SessionEntry::default()
    }
}

impl Default for SessionEntry {
    fn default() -> Self {
        SessionEntry {
            // Streaming is structural-only; the stored config's grouper is
            // ignored, and projections may override the mode per request.
            index: StreamingIndex::new(CompressorConfig::for_mode(Mode::Balanced)),
            last_touch: Instant::now(),
        }
    }
}

/// Shared, cloneable application state.
#[derive(Clone, Default)]
pub struct AppState {
    pub sessions: Arc<RwLock<HashMap<String, SessionEntry>>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState::default()
    }
}

// ---------------------------------------------------------------------------
// Line parsing (mirrors the CLI's `codag_compress` heuristic).
// ---------------------------------------------------------------------------

fn iso_or_epoch_re() -> &'static Regex {
    // ISO-8601 (date or datetime, optional ms/tz) or a long epoch (10-13 digits).
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"^(?:\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)?|\d{10,13})$",
        )
        .unwrap()
    })
}

const RAW_LEVELS: &[&str] = &[
    "error", "err", "warn", "warning", "fatal", "critical", "crit", "info", "debug", "trace",
    "notice",
];

fn is_level_tok(tok: &str) -> bool {
    let bare = tok
        .trim_matches(|c| "[](){}<>.,;:'\"".contains(c))
        .to_lowercase();
    RAW_LEVELS.contains(&bare.as_str())
}

/// Heuristically parse one raw text line into a [`LogLine`] (timestamp + level
/// among the first ~3 tokens, remainder = message). Faithful copy of the CLI's
/// `parse_raw_line`.
pub fn parse_line(line: &str) -> LogLine {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.is_empty() {
        return LogLine::new(String::new());
    }
    let mut timestamp: Option<String> = None;
    let mut level: Option<String> = None;
    let mut consumed = 0usize;

    // Leading timestamp.
    if iso_or_epoch_re().is_match(tokens[0]) {
        timestamp = Some(tokens[0].to_string());
        consumed = 1;
    } else if tokens.len() >= 2 {
        // Common "DATE TIME" two-token form: "2026-05-22 14:13:00".
        let joined = format!("{} {}", tokens[0], tokens[1]);
        if iso_or_epoch_re().is_match(&joined) {
            timestamp = Some(joined);
            consumed = 2;
        }
    }

    // Level among the next ~3 tokens (after any timestamp).
    let scan_end = (consumed + 3).min(tokens.len());
    for (i, tok) in tokens.iter().enumerate().take(scan_end).skip(consumed) {
        if is_level_tok(tok) {
            level = Some(
                tok.trim_matches(|c| "[](){}<>.,;:'\"".contains(c))
                    .to_lowercase(),
            );
            consumed = i + 1;
            break;
        }
    }

    let message = tokens[consumed..].join(" ");
    let message = if message.is_empty() && consumed == 0 {
        line.trim().to_string()
    } else {
        message
    };
    LogLine {
        message,
        level,
        timestamp,
    }
}

/// Parse one NDJSON line `{"message","level","timestamp"}` into a [`LogLine`].
/// Returns `None` if the line is not a JSON object with a string `message`.
pub fn parse_json_line(line: &str) -> Option<LogLine> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let message = v.get("message")?.as_str()?.to_string();
    let level = v
        .get("level")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let timestamp = v
        .get("timestamp")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    Some(LogLine {
        message,
        level,
        timestamp,
    })
}

/// Whether to treat a body as NDJSON or raw text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFormat {
    Text,
    Ndjson,
}

/// Parse a whole body (newline-delimited) into [`LogLine`]s, skipping blanks.
/// In NDJSON mode, lines that fail to parse are silently skipped.
pub fn parse_body(body: &str, fmt: BodyFormat) -> Vec<LogLine> {
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match fmt {
            BodyFormat::Text => Some(parse_line(l)),
            BodyFormat::Ndjson => parse_json_line(l),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_line_extracts_level() {
        let l = parse_line("ERROR db_pool exhausted waiting=12");
        assert_eq!(l.level.as_deref(), Some("error"));
        assert_eq!(l.message, "db_pool exhausted waiting=12");
        assert_eq!(l.timestamp, None);
    }

    #[test]
    fn parse_line_extracts_iso_timestamp_and_level() {
        let l = parse_line("2026-05-22T14:13:00Z WARN disk pressure high");
        assert_eq!(l.timestamp.as_deref(), Some("2026-05-22T14:13:00Z"));
        assert_eq!(l.level.as_deref(), Some("warn"));
        assert_eq!(l.message, "disk pressure high");
    }

    #[test]
    fn parse_line_date_then_time_and_level() {
        // The leading token "2026-05-22" already fully matches the ISO regex
        // (date-only form), so it is consumed as the timestamp; the time and
        // level then fall within the next-3-token level scan. This mirrors the
        // CLI's `parse_raw_line` behavior exactly.
        let l = parse_line("2026-05-22 14:13:00 info worker idle");
        assert_eq!(l.timestamp.as_deref(), Some("2026-05-22"));
        assert_eq!(l.level.as_deref(), Some("info"));
        assert_eq!(l.message, "worker idle");
    }

    #[test]
    fn parse_line_no_metadata() {
        let l = parse_line("acquired connection from pool, in_use=12");
        assert_eq!(l.level, None);
        assert_eq!(l.timestamp, None);
        assert_eq!(l.message, "acquired connection from pool, in_use=12");
    }

    #[test]
    fn parse_json_line_full() {
        let l = parse_json_line(r#"{"message":"boom","level":"error","timestamp":"t0"}"#).unwrap();
        assert_eq!(l.message, "boom");
        assert_eq!(l.level.as_deref(), Some("error"));
        assert_eq!(l.timestamp.as_deref(), Some("t0"));
    }

    #[test]
    fn parse_json_line_rejects_non_object() {
        assert!(parse_json_line("not json").is_none());
        assert!(parse_json_line(r#"{"no_message":1}"#).is_none());
    }

    #[test]
    fn parse_body_skips_blanks_text() {
        let lines = parse_body("a\n\n  \nb\n", BodyFormat::Text);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].message, "a");
        assert_eq!(lines[1].message, "b");
    }
}
