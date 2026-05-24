//! Deterministic LogHub-2.0 grouping eval (NO LLM).
//!
//! Validates codag-drain's deterministic grouping against the LogHub-2.0 human oracle
//! `EventTemplate` AND head-to-head against base `drain3_rust`. Both tests are
//! `#[ignore]` (real data, multi-second) and print tables; run with
//! `--ignored --nocapture`.
//!
//! ```text
//! cargo test -p codag-drain --test eval_loghub -- --ignored --nocapture
//! ```
//!
//! Env config:
//!   LOGHUB_DIR      required path to LogHub-2.0 structured CSV root
//!   LOGHUB_LIMIT    default 3000 lines/system
//!   LOGHUB_SYSTEMS  default Apache,BGL,HDFS,HPC,Hadoop,HealthApp,Linux,Mac,
//!                   OpenSSH,OpenStack,Proxifier,Spark,Thunderbird,Zookeeper
//!                   (whichever exist on disk are used)
//!   GITHUB_JSONL    required only for the private GitHub held-out eval
//!
//! Metrics are the standard LogHub-2.0 definitions (GA, FGA, FTA, purity); see
//! each helper's doc comment. Per-system + MEAN tables are printed for every
//! arm: codag-drain Drain-family variants, the generic statistical experiment,
//! and base `drain3`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use codag_drain::compress::grouper::{make_grouper, Group, Grouper, StatisticalGrouper};
use codag_drain::compress::profile::Profile;
use codag_drain::{template_logs, GrouperKind, LogLine, TemplaterConfig};

use drain3_rust::masking::default_masking_instructions;
use drain3_rust::{Drain, LogMasker};

const DEFAULT_SYSTEMS: &str =
    "Apache,BGL,HDFS,HPC,Hadoop,HealthApp,Linux,Mac,OpenSSH,OpenStack,Proxifier,Spark,Thunderbird,Zookeeper";
const DEFAULT_LIMIT: usize = 3000;

// ---------------------------------------------------------------------------
// Config + data loading
// ---------------------------------------------------------------------------

fn loghub_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("LOGHUB_DIR").expect("set LOGHUB_DIR to the LogHub-2.0 structured CSV root"),
    )
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

fn github_jsonl() -> PathBuf {
    PathBuf::from(
        std::env::var("GITHUB_JSONL").expect("set GITHUB_JSONL to the held-out GitHub JSONL file"),
    )
}

fn github_limit() -> usize {
    std::env::var("GITHUB_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX)
}

/// Extract the Python-repr log list embedded in the LoRA prompt. This is a
/// generic quoted-string scanner, not a log parser.
fn parse_prompt_log_list(prompt: &str) -> Vec<String> {
    let Some(label) = prompt.find("Log list:") else {
        return Vec::new();
    };
    let Some(open_rel) = prompt[label..].find('[') else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let bytes = prompt.as_bytes();
    let mut i = label + open_rel + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b']' {
            break;
        }
        if b != b'\'' && b != b'"' {
            i += 1;
            continue;
        }
        let quote = b;
        i += 1;
        let mut s = String::new();
        while i < bytes.len() {
            let c = bytes[i];
            if c == quote {
                i += 1;
                break;
            }
            if c == b'\\' && i + 1 < bytes.len() {
                let esc = bytes[i + 1];
                match esc {
                    b'n' => s.push('\n'),
                    b't' => s.push('\t'),
                    b'r' => s.push('\r'),
                    b'\\' => s.push('\\'),
                    b'\'' => s.push('\''),
                    b'"' => s.push('"'),
                    _ => s.push(esc as char),
                }
                i += 2;
            } else {
                s.push(c as char);
                i += 1;
            }
        }
        out.push(s);
    }
    out
}

/// Read GitHub LoRA held-out rows as `(log line, oracle template)` pairs.
fn read_github_rows(path: &Path, n: usize) -> Option<(Vec<String>, Vec<String>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut contents = Vec::new();
    let mut templates = Vec::new();
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let oracle = v.get("_oracle_template")?.as_str()?.to_string();
        let prompt = v
            .get("messages")?
            .as_array()?
            .first()?
            .get("content")?
            .as_str()?;
        for log in parse_prompt_log_list(prompt) {
            contents.push(log);
            templates.push(oracle.clone());
            if contents.len() >= n {
                return Some((contents, templates));
            }
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
/// - **purity:** for each predicted group, impure += size - max(per-oracle
///   count); purity = 1 - sum(impure)/n.
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

fn mean(vals: &[f64]) -> f64 {
    if vals.is_empty() {
        0.0
    } else {
        vals.iter().sum::<f64>() / vals.len() as f64
    }
}

fn bootstrap_mean_ci(vals: &[f64]) -> (f64, f64) {
    if vals.is_empty() {
        return (0.0, 0.0);
    }
    if vals.len() == 1 {
        return (vals[0], vals[0]);
    }

    let iters = 5000usize;
    let mut seed = 0xC0DA6DAD_u64 ^ vals.len() as u64;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let mut acc = 0.0;
        for _ in 0..vals.len() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let idx = ((seed >> 32) as usize) % vals.len();
            acc += vals[idx];
        }
        samples.push(acc / vals.len() as f64);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lo = samples[((iters as f64 * 0.025).floor() as usize).min(iters - 1)];
    let hi = samples[((iters as f64 * 0.975).floor() as usize).min(iters - 1)];
    (lo, hi)
}

fn bootstrap_mean_delta_ci(candidate: &[f64], baseline: &[f64]) -> (f64, f64, f64) {
    if candidate.is_empty() || baseline.is_empty() || candidate.len() != baseline.len() {
        return (0.0, 0.0, 0.0);
    }
    let mean_delta = candidate
        .iter()
        .zip(baseline)
        .map(|(c, b)| c - b)
        .sum::<f64>()
        / candidate.len() as f64;
    if candidate.len() == 1 {
        return (mean_delta, mean_delta, mean_delta);
    }

    let iters = 5000usize;
    let mut seed = 0xC0DA6DE17A_u64 ^ candidate.len() as u64;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let mut acc = 0.0;
        for _ in 0..candidate.len() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let idx = ((seed >> 32) as usize) % candidate.len();
            acc += candidate[idx] - baseline[idx];
        }
        samples.push(acc / candidate.len() as f64);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lo = samples[((iters as f64 * 0.025).floor() as usize).min(iters - 1)];
    let hi = samples[((iters as f64 * 0.975).floor() as usize).min(iters - 1)];
    (mean_delta, lo, hi)
}

fn metric_values(rows: &[Metrics], f: fn(Metrics) -> f64) -> Vec<f64> {
    rows.iter().copied().map(f).collect()
}

// ---------------------------------------------------------------------------
// Arms: turn each grouper / drain3 into a Vec<PredGroup>
// ---------------------------------------------------------------------------

/// Run one of our deterministic groupers and derive per-group templates through
/// the same `Profile` path used by `template_logs`.
fn pred_from_grouper(grouper: &dyn Grouper, lines: &[LogLine]) -> Vec<PredGroup> {
    let groups: Vec<Group> = grouper.group(lines);
    let profile = Profile::build(lines, &groups);
    groups
        .into_iter()
        .enumerate()
        .map(|(g_idx, g)| PredGroup {
            members: g.member_indices,
            template: profile.profiles[g_idx].template.clone(),
        })
        .collect()
}

fn pred_from_kind(kind: GrouperKind, lines: &[LogLine]) -> Vec<PredGroup> {
    let cfg = TemplaterConfig {
        grouper: kind,
        ..TemplaterConfig::default()
    };
    let grouper = make_grouper(&cfg);
    pred_from_grouper(grouper.as_ref(), lines)
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
        members_by_cluster
            .entry(cluster.cluster_id)
            .or_default()
            .push(i);
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

fn group_count_from_kind(kind: GrouperKind, lines: &[LogLine]) -> usize {
    let cfg = TemplaterConfig {
        grouper: kind,
        ..TemplaterConfig::default()
    };
    let grouper = make_grouper(&cfg);
    grouper.group(lines).len()
}

fn group_count_from_drain3(contents: &[String]) -> usize {
    let mut drain = Drain::new(4, 0.4, 100, None, vec![], "<*>".into(), true);
    drain.set_masker(LogMasker::new(default_masking_instructions()));
    let mut clusters = BTreeSet::new();
    for content in contents {
        let (cluster, _update) = drain.add_log_message(content);
        clusters.insert(cluster.cluster_id);
    }
    clusters.len()
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
    let arms = [
        "drain",
        "drain_stock",
        "drain_delim",
        "drain_full",
        "statistical",
        "drain3",
    ];
    let mut sums: HashMap<&str, (Metrics, f64, usize)> = HashMap::new(); // (metric sums, lines/s sum, n_systems)
    let mut per_system: HashMap<&str, Vec<Metrics>> = HashMap::new();

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
                "drain" => pred_from_kind(GrouperKind::Drain, &lines),
                "drain_stock" => pred_from_kind(GrouperKind::DrainStock, &lines),
                "drain_delim" => pred_from_kind(GrouperKind::DrainDelimited, &lines),
                "drain_full" => pred_from_kind(GrouperKind::DrainFullSearch, &lines),
                "statistical" => pred_from_grouper(&StatisticalGrouper::default(), &lines),
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
            per_system.entry(arm).or_default().push(m);
        }
    }

    assert!(
        any,
        "no systems produced rows; check LOGHUB_DIR={}",
        dir.display()
    );

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

    println!("=== Bootstrap 95% CI over systems (macro mean) ===");
    println!(
        "{:<11} {:>22} {:>22} {:>22} {:>22}",
        "grouper", "GA", "FGA", "FTA", "purity"
    );
    for &arm in &arms {
        let Some(rows) = per_system.get(arm) else {
            continue;
        };
        let ga = metric_values(rows, |m| m.ga);
        let fga = metric_values(rows, |m| m.fga);
        let fta = metric_values(rows, |m| m.fta);
        let purity = metric_values(rows, |m| m.purity);
        let (ga_lo, ga_hi) = bootstrap_mean_ci(&ga);
        let (fga_lo, fga_hi) = bootstrap_mean_ci(&fga);
        let (fta_lo, fta_hi) = bootstrap_mean_ci(&fta);
        let (pur_lo, pur_hi) = bootstrap_mean_ci(&purity);
        println!(
            "{:<11} {:>6.3} [{:>5.3},{:>5.3}] {:>6.3} [{:>5.3},{:>5.3}] {:>6.3} [{:>5.3},{:>5.3}] {:>6.3} [{:>5.3},{:>5.3}]",
            arm,
            mean(&ga),
            ga_lo,
            ga_hi,
            mean(&fga),
            fga_lo,
            fga_hi,
            mean(&fta),
            fta_lo,
            fta_hi,
            mean(&purity),
            pur_lo,
            pur_hi
        );
    }
    println!();

    let Some(base_rows) = per_system.get("drain3") else {
        return;
    };
    let base_ga = metric_values(base_rows, |m| m.ga);
    let base_fga = metric_values(base_rows, |m| m.fga);
    let base_fta = metric_values(base_rows, |m| m.fta);
    let base_purity = metric_values(base_rows, |m| m.purity);
    println!("=== Paired bootstrap delta vs drain3 (candidate - drain3) ===");
    println!(
        "{:<11} {:>22} {:>22} {:>22} {:>22}",
        "grouper", "GA delta", "FGA delta", "FTA delta", "purity delta"
    );
    for &arm in &arms {
        if arm == "drain3" {
            continue;
        }
        let Some(rows) = per_system.get(arm) else {
            continue;
        };
        let ga = metric_values(rows, |m| m.ga);
        let fga = metric_values(rows, |m| m.fga);
        let fta = metric_values(rows, |m| m.fta);
        let purity = metric_values(rows, |m| m.purity);
        let (ga_m, ga_lo, ga_hi) = bootstrap_mean_delta_ci(&ga, &base_ga);
        let (fga_m, fga_lo, fga_hi) = bootstrap_mean_delta_ci(&fga, &base_fga);
        let (fta_m, fta_lo, fta_hi) = bootstrap_mean_delta_ci(&fta, &base_fta);
        let (pur_m, pur_lo, pur_hi) = bootstrap_mean_delta_ci(&purity, &base_purity);
        println!(
            "{:<11} {:+6.3} [{:+5.3},{:+5.3}] {:+6.3} [{:+5.3},{:+5.3}] {:+6.3} [{:+5.3},{:+5.3}] {:+6.3} [{:+5.3},{:+5.3}]",
            arm,
            ga_m,
            ga_lo,
            ga_hi,
            fga_m,
            fga_lo,
            fga_hi,
            fta_m,
            fta_lo,
            fta_hi,
            pur_m,
            pur_lo,
            pur_hi
        );
    }
    println!();
}

#[test]
#[ignore = "github held-out eval: run with --ignored --nocapture"]
fn grouping_github_lora() {
    let path = github_jsonl();
    let lim = github_limit();
    let (contents, oracle) = read_github_rows(&path, lim)
        .unwrap_or_else(|| panic!("no GitHub rows at {}", path.display()));
    assert!(!contents.is_empty(), "no GitHub rows at {}", path.display());
    let lines = to_lines(&contents);
    let n = contents.len();
    let arms = [
        "drain",
        "drain_stock",
        "drain_delim",
        "drain_full",
        "statistical",
        "drain3",
    ];

    println!();
    println!("=== GitHub held-out grouping eval (lines={n}, NO LLM) ===");
    println!(
        "{:<11} {:>7} {:>6} {:>6} {:>6} {:>7} {:>9}",
        "grouper", "lines", "GA", "FGA", "FTA", "purity", "lines/s"
    );

    for &arm in &arms {
        let start = Instant::now();
        let pred: Vec<PredGroup> = match arm {
            "drain" => pred_from_kind(GrouperKind::Drain, &lines),
            "drain_stock" => pred_from_kind(GrouperKind::DrainStock, &lines),
            "drain_delim" => pred_from_kind(GrouperKind::DrainDelimited, &lines),
            "drain_full" => pred_from_kind(GrouperKind::DrainFullSearch, &lines),
            "statistical" => pred_from_grouper(&StatisticalGrouper::default(), &lines),
            "drain3" => pred_from_drain3(&contents),
            _ => unreachable!(),
        };
        let secs = start.elapsed().as_secs_f64();
        let lps = if secs > 0.0 { n as f64 / secs } else { 0.0 };
        let m = compute_metrics(&pred, &oracle);
        println!(
            "{:<11} {:>7} {:>6.3} {:>6.3} {:>6.3} {:>7.3} {:>9.0}",
            arm, n, m.ga, m.fga, m.fta, m.purity, lps
        );
    }
    println!();
}

#[test]
#[ignore = "loghub timing eval: real data, run with --ignored --nocapture"]
fn timing_loghub_default_vs_drain3() {
    let dir = loghub_dir();
    let lim = limit();
    let syss = systems();
    let arms = [
        "drain_group",
        "drain_stock_group",
        "drain_render",
        "drain3_group",
    ];
    let mut sums: HashMap<&str, (f64, usize)> = HashMap::new();
    let mut per_system: HashMap<&str, Vec<f64>> = HashMap::new();

    println!();
    println!("=== LogHub-2.0 timing eval (limit={lim}/system) ===");
    println!(
        "{:<11} {:<17} {:>7} {:>9} {:>9}",
        "system", "mode", "lines", "groups", "lines/s"
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
        let n = contents.len();

        for &arm in &arms {
            let start = Instant::now();
            let groups = match arm {
                "drain_group" => group_count_from_kind(GrouperKind::Drain, &lines),
                "drain_stock_group" => group_count_from_kind(GrouperKind::DrainStock, &lines),
                "drain_render" => template_logs(&lines, &TemplaterConfig::default()).template_count,
                "drain3_group" => group_count_from_drain3(&contents),
                _ => unreachable!(),
            };
            let secs = start.elapsed().as_secs_f64();
            let lps = if secs > 0.0 { n as f64 / secs } else { 0.0 };
            println!(
                "{:<11} {:<17} {:>7} {:>9} {:>9.0}",
                system, arm, n, groups, lps
            );
            let entry = sums.entry(arm).or_insert((0.0, 0));
            entry.0 += lps;
            entry.1 += 1;
            per_system.entry(arm).or_default().push(lps);
        }
    }

    assert!(
        any,
        "no systems produced rows; check LOGHUB_DIR={}",
        dir.display()
    );

    println!("{}", "-".repeat(62));
    for &arm in &arms {
        if let Some((lps, c)) = sums.get(arm) {
            if *c == 0 {
                continue;
            }
            println!(
                "{:<11} {:<17} {:>7} {:>9} {:>9.0}",
                "MEAN",
                arm,
                "",
                "",
                lps / *c as f64
            );
        }
    }
    println!();

    println!("=== Bootstrap 95% CI over systems (macro mean lines/s) ===");
    println!("{:<17} {:>26}", "mode", "lines/s");
    for &arm in &arms {
        let Some(vals) = per_system.get(arm) else {
            continue;
        };
        let (lo, hi) = bootstrap_mean_ci(vals);
        println!("{:<17} {:>9.0} [{:>9.0},{:>9.0}]", arm, mean(vals), lo, hi);
    }
    println!();

    let Some(drain_group) = per_system.get("drain_group") else {
        return;
    };
    let Some(drain3_group) = per_system.get("drain3_group") else {
        return;
    };
    let Some(drain_render) = per_system.get("drain_render") else {
        return;
    };
    let group_ratio: Vec<f64> = drain_group
        .iter()
        .zip(drain3_group)
        .map(|(ours, base)| ours / base)
        .collect();
    let render_ratio: Vec<f64> = drain_render
        .iter()
        .zip(drain3_group)
        .map(|(ours, base)| ours / base)
        .collect();
    let (group_lo, group_hi) = bootstrap_mean_ci(&group_ratio);
    let (render_lo, render_hi) = bootstrap_mean_ci(&render_ratio);
    println!(
        "drain_group / drain3_group speed ratio: {:.3} [{:.3},{:.3}]",
        mean(&group_ratio),
        group_lo,
        group_hi
    );
    println!(
        "drain_render / drain3_group speed ratio: {:.3} [{:.3},{:.3}]",
        mean(&render_ratio),
        render_lo,
        render_hi
    );
    println!();
}

// ---------------------------------------------------------------------------
// Test 2: real-data template compression (codag_drain::template_logs)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "loghub template compression: real data, run with --ignored --nocapture"]
fn compression_loghub() {
    let dir = loghub_dir();
    let lim = limit();
    let syss = systems();

    let arms = [
        ("drain", GrouperKind::Drain),
        ("drain_stock", GrouperKind::DrainStock),
        ("drain_delim", GrouperKind::DrainDelimited),
        ("drain_full", GrouperKind::DrainFullSearch),
        ("statistical", GrouperKind::Statistical),
    ];
    // arm name -> (line-compression sum, char-compression sum, n_systems)
    let mut sums: HashMap<&str, (f64, f64, usize)> = HashMap::new();
    let mut per_system: HashMap<&str, Vec<(f64, f64)>> = HashMap::new();

    println!();
    println!(
        "=== LogHub-2.0 template compression (limit={lim}/system, codag_drain::template_logs) ==="
    );
    println!(
        "{:<11} {:<10} {:>7} {:>9} {:>9} {:>9} {:>9}",
        "system", "grouper", "lines", "templates", "line_cx", "in_chars", "char_cx"
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

        for (name, grouper) in &arms {
            let cfg = TemplaterConfig {
                grouper: *grouper,
                ..TemplaterConfig::default()
            };
            let result = template_logs(&lines, &cfg);
            let rendered = result.render();
            let out_lines = result.template_count;
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
            per_system.entry(name).or_default().push((line_cx, char_cx));
        }
    }

    assert!(
        any,
        "no systems produced rows; check LOGHUB_DIR={}",
        dir.display()
    );

    println!("{}", "-".repeat(66));
    for (name, _grouper) in &arms {
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

    println!("=== Bootstrap 95% CI over systems (macro mean) ===");
    println!("{:<11} {:>24} {:>24}", "grouper", "line_cx", "char_cx");
    for (name, _grouper) in &arms {
        let Some(rows) = per_system.get(name) else {
            continue;
        };
        let line_vals: Vec<f64> = rows.iter().map(|r| r.0).collect();
        let char_vals: Vec<f64> = rows.iter().map(|r| r.1).collect();
        let (line_lo, line_hi) = bootstrap_mean_ci(&line_vals);
        let (char_lo, char_hi) = bootstrap_mean_ci(&char_vals);
        println!(
            "{:<11} {:>7.2}x [{:>6.2},{:>6.2}] {:>7.2}x [{:>6.2},{:>6.2}]",
            name,
            mean(&line_vals),
            line_lo,
            line_hi,
            mean(&char_vals),
            char_lo,
            char_hi
        );
    }
    println!();
}

#[test]
#[ignore = "github held-out template compression: run with --ignored --nocapture"]
fn compression_github_lora() {
    let path = github_jsonl();
    let lim = github_limit();
    let (contents, _oracle) = read_github_rows(&path, lim)
        .unwrap_or_else(|| panic!("no GitHub rows at {}", path.display()));
    assert!(!contents.is_empty(), "no GitHub rows at {}", path.display());

    let lines = to_lines(&contents);
    let in_chars: usize = contents.iter().map(|c| c.len()).sum::<usize>() + contents.len();
    let n = contents.len();
    let arms = [
        ("drain", GrouperKind::Drain),
        ("drain_stock", GrouperKind::DrainStock),
        ("drain_delim", GrouperKind::DrainDelimited),
        ("drain_full", GrouperKind::DrainFullSearch),
        ("statistical", GrouperKind::Statistical),
    ];

    println!();
    println!(
        "=== GitHub held-out template compression (lines={n}, codag_drain::template_logs) ==="
    );
    println!(
        "{:<11} {:>7} {:>9} {:>9} {:>9} {:>9}",
        "grouper", "lines", "templates", "line_cx", "in_chars", "char_cx"
    );

    for (name, grouper) in &arms {
        let cfg = TemplaterConfig {
            grouper: *grouper,
            ..TemplaterConfig::default()
        };
        let result = template_logs(&lines, &cfg);
        let rendered = result.render();
        let out_lines = result.template_count;
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
            "{:<10} {:>7} {:>9} {:>9.2} {:>9} {:>9.2}",
            name, n, out_lines, line_cx, in_chars, char_cx
        );
    }
    println!();
}
