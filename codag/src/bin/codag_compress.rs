//! `codag_compress` — pipe logs through the deterministic compressor.
//!
//! Usage:
//!   echo logs | codag_compress [--json] [--grouper structural|adaptive|fixed]
//!                              [--mode lossless|balanced|aggressive]
//!
//! Default stdin = raw text: heuristically parse a leading ISO-8601/epoch
//! timestamp + a level token among the first ~3 tokens; remainder = message.
//! `--json`: each stdin line is `{"message","level","timestamp"}`.
//!
//! Prints `result.render()` to stdout; one-line stats to stderr.

use std::io::{self, Read, Write};

use codag::compress::{compress, CompressorConfig, GrouperKind, LogLine, Mode};
use regex::Regex;
use std::sync::OnceLock;

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

/// Heuristically parse one raw line into a `LogLine`.
fn parse_raw_line(line: &str) -> LogLine {
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

/// Parse one JSON line `{"message","level","timestamp"}`.
fn parse_json_line(line: &str) -> Option<LogLine> {
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut json = false;
    let mut grouper: Option<GrouperKind> = None;
    let mut mode = Mode::Balanced;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--grouper" => {
                if let Some(v) = it.next() {
                    grouper = Some(match v.as_str() {
                        "structural" => GrouperKind::Structural,
                        "adaptive" => GrouperKind::Adaptive,
                        "fixed" => GrouperKind::Fixed,
                        other => {
                            eprintln!("[codag] unknown grouper: {other}");
                            std::process::exit(2);
                        }
                    });
                }
            }
            "--mode" => {
                if let Some(v) = it.next() {
                    mode = match v.as_str() {
                        "lossless" => Mode::Lossless,
                        "balanced" => Mode::Balanced,
                        "aggressive" => Mode::Aggressive,
                        other => {
                            eprintln!("[codag] unknown mode: {other}");
                            std::process::exit(2);
                        }
                    };
                }
            }
            "-h" | "--help" => {
                println!(
                    "echo logs | codag_compress [--json] \
[--grouper structural|adaptive|fixed] [--mode lossless|balanced|aggressive]"
                );
                return;
            }
            other => {
                eprintln!("[codag] unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }

    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("[codag] failed to read stdin");
        std::process::exit(1);
    }

    let lines: Vec<LogLine> = input
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            if json {
                parse_json_line(l)
            } else {
                Some(parse_raw_line(l))
            }
        })
        .collect();

    let mut config = CompressorConfig::for_mode(mode);
    if let Some(g) = grouper {
        config.grouper = g;
    }

    let result = compress(&lines, &config);
    let out = result.render();
    let stdout = io::stdout();
    let mut h = stdout.lock();
    let _ = writeln!(h, "{out}");

    let n = result.original_count;
    let m = result.lines.len();
    let ratio = result.line_compression();
    eprintln!(
        "[codag] {n} -> {m} lines ({ratio:.1}x), {} kept verbatim",
        result.kept_count
    );
}
