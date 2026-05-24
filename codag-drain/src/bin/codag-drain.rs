//! `codag-drain` - pipe logs through the deterministic templater.
//!
//! Usage:
//!   echo logs | codag-drain [--json]
//!                            [--grouper drain|drain-stock|drain-delimited|drain-fullsearch|statistical]
//!                            [--samples N] [--format text|json] [--stats]
//!
//! Default stdin = raw text: heuristically parse a leading ISO-8601/epoch
//! timestamp + a level token among the first ~3 tokens; remainder = message.
//! `--json`: each stdin line is `{"message","level","timestamp"}`.
//!
//! Prints template groups to stdout. `--stats` also prints a one-line summary
//! to stderr.

use std::io::{self, Read, Write};

use codag_drain::{
    parse_json_line, parse_line, template_logs, GrouperKind, LogLine, TemplaterConfig,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input_json = false;
    let mut output_json = false;
    let mut grouper: Option<GrouperKind> = None;
    let mut sample_cap: Option<usize> = None;
    let mut print_stats = false;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => input_json = true,
            "--stats" => print_stats = true,
            "--format" => {
                if let Some(v) = it.next() {
                    output_json = match v.as_str() {
                        "json" => true,
                        "text" => false,
                        other => {
                            eprintln!("[codag-drain] unknown format: {other}");
                            std::process::exit(2);
                        }
                    };
                }
            }
            "--grouper" => {
                if let Some(v) = it.next() {
                    grouper = Some(match v.as_str() {
                        "drain" => GrouperKind::Drain,
                        "drain-stock" | "stock" => GrouperKind::DrainStock,
                        "drain-delimited" | "delimited" => GrouperKind::DrainDelimited,
                        "drain-fullsearch" | "fullsearch" => GrouperKind::DrainFullSearch,
                        "statistical" => GrouperKind::Statistical,
                        other => {
                            eprintln!("[codag-drain] unknown grouper: {other}");
                            std::process::exit(2);
                        }
                    });
                }
            }
            "--samples" => {
                if let Some(v) = it.next() {
                    sample_cap = Some(match v.parse::<usize>() {
                        Ok(n) => n,
                        Err(_) => {
                            eprintln!("[codag-drain] invalid samples value: {v}");
                            std::process::exit(2);
                        }
                    });
                }
            }
            "-h" | "--help" => {
                println!(
                    "echo logs | codag-drain [--json] \
[--grouper drain|drain-stock|drain-delimited|drain-fullsearch|statistical] \
[--samples N] [--format text|json] [--stats]"
                );
                return;
            }
            other => {
                eprintln!("[codag-drain] unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }

    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("[codag-drain] failed to read stdin");
        std::process::exit(1);
    }

    let lines: Vec<LogLine> = input
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            if input_json {
                parse_json_line(l)
            } else {
                Some(parse_line(l))
            }
        })
        .collect();

    let mut config = TemplaterConfig::default();
    if let Some(g) = grouper {
        config.grouper = g;
    }
    if let Some(n) = sample_cap {
        config.sample_cap = n;
    }

    let result = template_logs(&lines, &config);
    let out = if output_json {
        serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string())
    } else {
        result.render()
    };
    let stdout = io::stdout();
    let mut h = stdout.lock();
    let _ = writeln!(h, "{out}");

    if print_stats {
        let n = result.original_count;
        let m = result.template_count;
        let ratio = result.line_compression;
        eprintln!("[codag-drain] {n} lines -> {m} templates ({ratio:.1}x)");
    }
}
