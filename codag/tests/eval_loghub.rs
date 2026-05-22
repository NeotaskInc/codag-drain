//! Phase 4 — deterministic LogHub-2.0 grouping eval (NO LLM).
//!
//! Validates codag's deterministic grouping against the LogHub-2.0 human oracle
//! `EventTemplate` AND head-to-head against base `drain3_rust`. Both tests are
//! `#[ignore]` (real data, multi-second) and print tables; run with
//! `--ignored --nocapture`.
//!
//! ```text
//! cargo test -p codag --test eval_loghub -- --ignored --nocapture
//! ```
//!
//! Env config:
//!   LOGHUB_DIR      default below (the infra-logs checkout)
//!   LOGHUB_LIMIT    default 3000 lines/system
//!   LOGHUB_SYSTEMS  default Hadoop,HDFS,BGL,Spark,Zookeeper,OpenSSH,Linux,Proxifier
//!                   (whichever exist on disk are used)
//!
//! Metrics are the standard LogHub-2.0 definitions (GA, FGA, FTA, purity); see
//! each helper's doc comment. Per-system + MEAN tables are printed for every
//! arm: codag `structural`, codag `adaptive`, codag `fixed`, and base `drain3`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use codag::compress::grouper::{
    FixedDepthTree, Group, Grouper, GuardedAdaptiveGrouper, StructuralExactGrouper,
};
use codag::compress::guard::Guard;
use codag::compress::normalize::Normalizer;
use codag::compress::template::derive_multi_template;
use codag::{compress, CompressorConfig, LogLine, Mode};

use drain3_rust::masking::default_masking_instructions;
use drain3_rust::{Drain, LogMasker};

const DEFAULT_DIR: &str =
    "/Users/michael/Desktop/Workspace/Startup/codag-org/infra-logs/v2/data/loghub2";
const DEFAULT_SYSTEMS: &str = "Hadoop,HDFS,BGL,Spark,Zookeeper,OpenSSH,Linux,Proxifier";
const DEFAULT_LIMIT: usize = 3000;

// ---------------------------------------------------------------------------
// Config + data loading
// ---------------------------------------------------------------------------

fn loghub_dir() -> PathBuf {
    PathBuf::from(std::env::var("LOGHUB_DIR").unwrap_or_else(|_| DEFAULT_DIR.to_string()))
}

fn limit() -> usize {
    std::env::var("LOGHUB_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_LIMIT)
}

fn systems() -> Vec<String> {
    std::env::var("LOGHUB_SYSTEMS")
        .unwrap_or_else(|_| DEFAULT_SYSTEMS.to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn csv_path(dir: &Path, system: &str) -> PathBuf {
    dir.join(system)
        .join(format!("{system}_full.log_structured.csv"))
}

/// Read up to `n` `(Content, EventTemplate)` rows from a LogHub-2.0 structured
/// CSV. Streams the file so the multi-GB systems (HDFS, BGL) never fully load.
fn read_rows(path: &Path, n: usize) -> Option<(Vec<String>, Vec<String>)> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_path(path)
        .ok()?;
    let headers = rdr.headers().ok()?.clone();
    let content_idx = headers.iter().position(|h| h == "Content")?;
    let template_idx = headers.iter().position(|h| h == "EventTemplate")?;

    let mut contents = Vec::with_capacity(n.min(4096));
    let mut templates = Vec::with_capacity(n.min(4096));
    for rec in rdr.records() {
        let rec = match rec {
            Ok(r) => r,
            Err(_) => continue,
        };
        let (c, t) = match (rec.get(content_idx), rec.get(template_idx)) {
            (Some(c), Some(t)) => (c.to_string(), t.to_string()),
            _ => continue,
        };
        contents.push(c);
        templates.push(t);
        if contents.len() >= n {
            break;
        }
    }
    Some((contents, templates))
}

// ---------------------------------------------------------------------------
// Predicted-group representation + the standard LogHub-2.0 metrics
// ---------------------------------------------------------------------------

/// One predicted group: its (sorted) member line-indices and a derived template.
struct PredGroup {
    members: Vec<usize>,
    template: String,
}

/// Whitespace-normalize a template string: collapse runs of whitespace to a
/// single space, then trim. Used for FTA string comparison.
fn norm_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Copy, Default)]
struct Metrics {
    ga: f64,
    fga: f64,
    fta: f64,
    purity: f64,
}

/// Compute GA, FGA, FTA, purity for a set of predicted groups against the
/// per-line oracle `EventTemplate` strings.
///
/// - **GA (line-weighted Group Accuracy):** an oracle template's lines are
///   "correct" iff that exact line-index set is one of the predicted member
///   sets; GA = sum(|oracle_set| for correct oracle templates) / n.
/// - **purity:** for each predicted group, impure += size − max(per-oracle
///   count); purity = 1 − sum(impure)/n.
/// - **FGA:** a predicted group is correct iff its member-set equals some
///   oracle template's member-set. p = correct/n_pred, r = correct/n_oracle,
///   FGA = 2pr/(p+r).
/// - **FTA:** like FGA but ALSO require predicted template string ==
///   oracle template string (both whitespace-normalized).
fn compute_metrics(pred: &[PredGroup], oracle: &[String]) -> Metrics {
    let n = oracle.len();
    if n == 0 {
        return Metrics::default();
    }

    // oracle template -> set of line indices (and -> normalized template string).
    let mut oracle_sets: HashMap<&str, BTreeSet<usize>> = HashMap::new();
    for (i, t) in oracle.iter().enumerate() {
        oracle_sets.entry(t.as_str()).or_default().insert(i);
    }
    let n_oracle = oracle_sets.len();

    // Predicted member-sets (as BTreeSet for equality), and the lookup for FGA/FTA.
    // A set value -> the matching oracle template string (if its set equals one).
    let mut oracle_set_to_template: HashMap<BTreeSet<usize>, String> = HashMap::new();
    for (t, set) in &oracle_sets {
        oracle_set_to_template.insert(set.clone(), norm_ws(t));
    }
    let pred_set_values: BTreeSet<BTreeSet<usize>> = pred
        .iter()
        .map(|g| g.members.iter().copied().collect::<BTreeSet<usize>>())
        .collect();

    // GA: line-weighted over oracle templates whose set is exactly a predicted set.
    let oracle_lines_correct: usize = oracle_sets
        .values()
        .filter(|set| pred_set_values.contains(*set))
        .map(|set| set.len())
        .sum();
    let ga = oracle_lines_correct as f64 / n as f64;

    // purity: per predicted group, size - max oracle-template count within it.
    let mut impure = 0usize;
    for g in pred {
        if g.members.is_empty() {
            continue;
        }
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for &m in &g.members {
            *counts.entry(oracle[m].as_str()).or_insert(0) += 1;
        }
        let max_count = counts.values().copied().max().unwrap_or(0);
        impure += g.members.len() - max_count;
    }
    let purity = 1.0 - (impure as f64 / n as f64);

    // FGA / FTA: count predicted groups whose member-set equals an oracle set
    // (FGA), additionally requiring the template string to match (FTA).
    let n_pred = pred.len();
    let mut correct_set = 0usize;
    let mut correct_set_and_str = 0usize;
    for g in pred {
        let set: BTreeSet<usize> = g.members.iter().copied().collect();
        if let Some(oracle_tmpl) = oracle_set_to_template.get(&set) {
            correct_set += 1;
            if norm_ws(&g.template) == *oracle_tmpl {
                correct_set_and_str += 1;
            }
        }
    }
    let fga = f1(correct_set, n_pred, n_oracle);
    let fta = f1(correct_set_and_str, n_pred, n_oracle);

    Metrics {
        ga,
        fga,
        fta,
        purity,
    }
}

/// F1 over (correct / n_pred) precision and (correct / n_oracle) recall.
fn f1(correct: usize, n_pred: usize, n_oracle: usize) -> f64 {
    if n_pred == 0 || n_oracle == 0 {
        return 0.0;
    }
    let p = correct as f64 / n_pred as f64;
    let r = correct as f64 / n_oracle as f64;
    if p + r == 0.0 {
        0.0
    } else {
        2.0 * p * r / (p + r)
    }
}

// ---------------------------------------------------------------------------
// Arms: turn each grouper / drain3 into a Vec<PredGroup>
// ---------------------------------------------------------------------------

/// Run one of our deterministic groupers and derive a per-group template the
/// same way `Profile` does: per-member `normalize_for_grouping`, then
/// `derive_multi_template` (which itself falls back to a pair derivation for
/// ragged groups).
fn pred_from_grouper(grouper: &dyn Grouper, lines: &[LogLine]) -> Vec<PredGroup> {
    let norm = Normalizer::new();
    let guard = Guard::new();
    let groups: Vec<Group> = grouper.group(lines, &norm, &guard);
    groups
        .into_iter()
        .map(|g| {
            let members_norm: Vec<Vec<String>> = g
                .member_indices
                .iter()
                .map(|&m| norm.normalize_for_grouping(&lines[m].message))
                .collect();
            let a_raw = &lines[g.member_indices[0]].message;
            let template = derive_multi_template(a_raw, &members_norm);
            PredGroup {
                members: g.member_indices,
                template,
            }
        })
        .collect()
}

/// Run base drain3 over the lines: each line's returned `cluster.cluster_id` is
/// the predicted group; the final per-cluster `get_template()` (last seen) is
/// the template used for FTA.
fn pred_from_drain3(contents: &[String]) -> Vec<PredGroup> {
    let mut drain = Drain::new(4, 0.4, 100, None, vec![], "<*>".into(), true);
    drain.set_masker(LogMasker::new(default_masking_instructions()));

    // cluster_id -> member line indices (insertion order preserved).
    let mut members_by_cluster: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut template_by_cluster: BTreeMap<usize, String> = BTreeMap::new();
    for (i, content) in contents.iter().enumerate() {
        let (cluster, _update) = drain.add_log_message(content);
        members_by_cluster.entry(cluster.cluster_id).or_default().push(i);
        // last-seen template wins.
        template_by_cluster.insert(cluster.cluster_id, cluster.get_template());
    }

    members_by_cluster
        .into_iter()
        .map(|(cid, members)| PredGroup {
            template: template_by_cluster.get(&cid).cloned().unwrap_or_default(),
            members,
        })
        .collect()
}

fn to_lines(contents: &[String]) -> Vec<LogLine> {
    contents.iter().map(|c| LogLine::new(c.clone())).collect()
}

// ---------------------------------------------------------------------------
// Test 1: grouping eval (ours vs drain3, vs the oracle)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loghub eval: real data, run with --ignored --nocapture"]
fn grouping_loghub() {
    let dir = loghub_dir();
    let lim = limit();
    let syss = systems();

    // arm name -> running sums for the MEAN row.
    let arms = ["structural", "adaptive", "fixed", "drain3"];
    let mut sums: HashMap<&str, (Metrics, f64, usize)> = HashMap::new(); // (metric sums, lines/s sum, n_systems)

    println!();
    println!("=== LogHub-2.0 grouping eval (limit={lim}/system, NO LLM) ===");
    println!(
        "{:<11} {:<11} {:>7} {:>6} {:>6} {:>6} {:>7} {:>9}",
        "system", "grouper", "lines", "GA", "FGA", "FTA", "purity", "lines/s"
    );

    let mut any = false;
    for system in &syss {
        let path = csv_path(&dir, system);
        let (contents, oracle) = match read_rows(&path, lim) {
            Some(v) if !v.0.is_empty() => v,
            _ => {
                eprintln!("skip {system}: no rows at {}", path.display());
                continue;
            }
        };
        any = true;
        let lines = to_lines(&contents);
        let n = contents.len();

        for &arm in &arms {
            let start = Instant::now();
            let pred: Vec<PredGroup> = match arm {
                "structural" => pred_from_grouper(&StructuralExactGrouper, &lines),
                // Canonical adaptive defaults (same as `make_grouper(Adaptive, ..)`).
                "adaptive" => pred_from_grouper(
                    &GuardedAdaptiveGrouper {
                        max_diff: 2,
                        sample: 64,
                    },
                    &lines,
                ),
                "fixed" => pred_from_grouper(&FixedDepthTree::new(3, 0.5), &lines),
                "drain3" => pred_from_drain3(&contents),
                _ => unreachable!(),
            };
            let secs = start.elapsed().as_secs_f64();
            let lps = if secs > 0.0 { n as f64 / secs } else { 0.0 };
            let m = compute_metrics(&pred, &oracle);

            println!(
                "{:<11} {:<11} {:>7} {:>6.3} {:>6.3} {:>6.3} {:>7.3} {:>9.0}",
                system, arm, n, m.ga, m.fga, m.fta, m.purity, lps
            );

            let entry = sums.entry(arm).or_insert((Metrics::default(), 0.0, 0));
            entry.0.ga += m.ga;
            entry.0.fga += m.fga;
            entry.0.fta += m.fta;
            entry.0.purity += m.purity;
            entry.1 += lps;
            entry.2 += 1;
        }
    }

    assert!(any, "no systems produced rows; check LOGHUB_DIR={}", dir.display());

    println!("{}", "-".repeat(72));
    for &arm in &arms {
        if let Some((m, lps, c)) = sums.get(arm) {
            if *c == 0 {
                continue;
            }
            let cf = *c as f64;
            println!(
                "{:<11} {:<11} {:>7} {:>6.3} {:>6.3} {:>6.3} {:>7.3} {:>9.0}",
                "MEAN",
                arm,
                "",
                m.ga / cf,
                m.fga / cf,
                m.fta / cf,
                m.purity / cf,
                lps / cf
            );
        }
    }
    println!();

    // Soft sanity (informational; don't fail the eval on real-data variance).
    if let (Some(s), Some(d)) = (sums.get("structural"), sums.get("drain3")) {
        if s.2 > 0 && d.2 > 0 {
            let s_pur = s.0.purity / s.2 as f64;
            let d_pur = d.0.purity / d.2 as f64;
            eprintln!(
                "[sanity] structural purity {s_pur:.3} (expect ~1.000) vs drain3 {d_pur:.3} (expect <1.0)"
            );
        }
    }
    if let (Some(a), Some(d)) = (sums.get("adaptive"), sums.get("drain3")) {
        if a.2 > 0 && d.2 > 0 {
            let a_ga = a.0.ga / a.2 as f64;
            let d_ga = d.0.ga / d.2 as f64;
            eprintln!("[sanity] adaptive mean GA {a_ga:.3} vs drain3 mean GA {d_ga:.3} (expect adaptive >= drain3)");
        }
    }
}

// ---------------------------------------------------------------------------
// Test 2: real-data compression (codag::compress, Lossless + Balanced)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loghub compression: real data, run with --ignored --nocapture"]
fn compression_loghub() {
    let dir = loghub_dir();
    let lim = limit();
    let syss = systems();

    let modes = [("lossless", Mode::Lossless), ("balanced", Mode::Balanced)];
    // mode name -> (line-compression sum, char-compression sum, n_systems)
    let mut sums: HashMap<&str, (f64, f64, usize)> = HashMap::new();

    println!();
    println!("=== LogHub-2.0 compression (limit={lim}/system, codag::compress) ===");
    println!(
        "{:<11} {:<10} {:>7} {:>9} {:>9} {:>9} {:>9}",
        "system", "mode", "lines", "out", "line_cx", "in_chars", "char_cx"
    );

    let mut any = false;
    for system in &syss {
        let path = csv_path(&dir, system);
        let (contents, _oracle) = match read_rows(&path, lim) {
            Some(v) if !v.0.is_empty() => v,
            _ => {
                eprintln!("skip {system}: no rows at {}", path.display());
                continue;
            }
        };
        any = true;
        let lines = to_lines(&contents);
        let in_chars: usize = contents.iter().map(|c| c.len()).sum::<usize>() + contents.len(); // +newlines
        let n = contents.len();

        for (name, mode) in &modes {
            let cfg = CompressorConfig::for_mode(*mode);
            let result = compress(&lines, &cfg);
            let rendered = result.render();
            let out_lines = result.lines.len();
            let out_chars = rendered.len();
            let line_cx = if out_lines > 0 {
                n as f64 / out_lines as f64
            } else {
                0.0
            };
            let char_cx = if out_chars > 0 {
                in_chars as f64 / out_chars as f64
            } else {
                0.0
            };

            println!(
                "{:<11} {:<10} {:>7} {:>9} {:>9.2} {:>9} {:>9.2}",
                system, name, n, out_lines, line_cx, in_chars, char_cx
            );

            let entry = sums.entry(name).or_insert((0.0, 0.0, 0));
            entry.0 += line_cx;
            entry.1 += char_cx;
            entry.2 += 1;
        }
    }

    assert!(any, "no systems produced rows; check LOGHUB_DIR={}", dir.display());

    println!("{}", "-".repeat(66));
    for (name, _mode) in &modes {
        if let Some((lcx, ccx, c)) = sums.get(name) {
            if *c == 0 {
                continue;
            }
            let cf = *c as f64;
            println!(
                "{:<11} {:<10} {:>7} {:>9} {:>9.2} {:>9} {:>9.2}",
                "MEAN",
                name,
                "",
                "",
                lcx / cf,
                "",
                ccx / cf
            );
        }
    }
    println!();
}
