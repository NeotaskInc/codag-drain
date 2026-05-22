//! Deterministic, evidence-preserving log compressor (Phase 1).
//!
//! Pipeline: `LogLine`s -> grouper (structural / fixed / adaptive) -> per-group
//! `Profile` (template + per-slot numeric stats) -> NEW keep-policy -> rendered
//! `CompressionResult`.
//!
//! The grouping / normalization / role rules are faithful ports of the v2
//! Python pipeline (see submodules). The keep-policy here is NEW (it is *not*
//! the policy in `det_compressors.py`; that one is intentionally ignored).

pub mod grouper;
pub mod guard;
pub mod normalize;
pub mod profile;
pub mod role;
pub mod template;

use grouper::make_grouper;
use guard::Guard;
use normalize::Normalizer;
use profile::{distinct_samples, Profile};
use role::RoleClassifier;

/// Default minimum static (non-placeholder) chars for a useful template.
pub const DEFAULT_MIN_STATIC_CHARS: usize = 3;
/// Minimum hidden members before a group is collapsed (vs treated as rare).
pub const MIN_COLLAPSE: usize = 3;
/// Cap on distinct non-numeric samples shown per slot.
const SAMPLE_CAP: usize = 6;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single log line. `level` / `timestamp` are optional metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub message: String,
    pub level: Option<String>,
    pub timestamp: Option<String>,
}

impl LogLine {
    /// Construct a line with no level/timestamp.
    pub fn new(message: String) -> Self {
        LogLine {
            message,
            level: None,
            timestamp: None,
        }
    }
}

/// Which grouper to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GrouperKind {
    #[default]
    Structural,
    Adaptive,
    Fixed,
}

/// Compression aggressiveness preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    Lossless,
    #[default]
    Balanced,
    Aggressive,
}

/// Tunable knobs for the compressor.
#[derive(Debug, Clone)]
pub struct CompressorConfig {
    pub grouper: GrouperKind,
    pub prefix_len: usize,
    pub jaccard_th: f64,
    pub template_clip: usize,
    pub outlier_factor: f64,
    pub min_static_chars: usize,
    pub signal_levels: Vec<String>,
    pub signal_sample_cap: usize,
    pub max_rare_lines: usize,
}

fn default_signal_levels() -> Vec<String> {
    ["error", "err", "warn", "warning", "fatal", "critical", "crit"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Default for CompressorConfig {
    /// Balanced defaults.
    fn default() -> Self {
        CompressorConfig::for_mode(Mode::Balanced)
    }
}

impl CompressorConfig {
    /// Map a `Mode` preset onto the knobs.
    pub fn for_mode(mode: Mode) -> Self {
        let (grouper, signal_sample_cap, max_rare_lines) = match mode {
            Mode::Lossless => (GrouperKind::Structural, usize::MAX, usize::MAX),
            Mode::Balanced => (GrouperKind::Adaptive, 4, 60),
            Mode::Aggressive => (GrouperKind::Adaptive, 2, 20),
        };
        CompressorConfig {
            grouper,
            prefix_len: 3,
            jaccard_th: 0.5,
            template_clip: 70,
            outlier_factor: 4.0,
            min_static_chars: DEFAULT_MIN_STATIC_CHARS,
            signal_levels: default_signal_levels(),
            signal_sample_cap,
            max_rare_lines,
        }
    }
}

/// Deterministic role tag (subset modeled by the override rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    None,
    Routine,
    Context,
    Consequence,
}

/// Per-slot statistics rendered inside a collapsed line.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotStat {
    pub numeric: bool,
    pub min: f64,
    pub max: f64,
    pub median: f64,
    pub unit: String,
    pub distinct: usize,
    pub samples: Vec<String>,
}

/// One output line: either kept verbatim or a collapsed group/tail summary.
#[derive(Debug, Clone, PartialEq)]
pub enum OutputLine {
    Kept {
        index: usize,
        text: String,
        timestamp: Option<String>,
    },
    Collapsed {
        first_index: usize,
        count: usize,
        template: String,
        slots: Vec<SlotStat>,
        timestamp: Option<String>,
    },
}

/// The compression result.
#[derive(Debug, Clone)]
pub struct CompressionResult {
    pub lines: Vec<OutputLine>,
    pub original_count: usize,
    pub kept_count: usize,
}

impl CompressionResult {
    /// Render to a newline-joined string.
    pub fn render(&self) -> String {
        self.lines
            .iter()
            .map(render_line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// original_count / number of output lines (line compression factor).
    pub fn line_compression(&self) -> f64 {
        if self.lines.is_empty() {
            return 0.0;
        }
        self.original_count as f64 / self.lines.len() as f64
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Format an f64 without trailing `.0` noise (`20.0` -> `20`, `0.8` -> `0.8`).
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.is_finite() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        // Trim trailing zeros but keep at least one fractional digit.
        let mut s = format!("{v}");
        if s.contains('.') {
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        s
    }
}

/// Render one slot's summary fragment.
fn fmt_slot(slot: &SlotStat) -> String {
    if slot.numeric {
        let unit = &slot.unit;
        if slot.min == slot.max {
            format!("[{}{}]", fmt_num(slot.min), unit)
        } else {
            format!(
                "[{}..{}{} p50={}{}]",
                fmt_num(slot.min),
                fmt_num(slot.max),
                unit,
                fmt_num(slot.median),
                unit
            )
        }
    } else if slot.distinct <= slot.samples.len() {
        format!("[{}]", slot.samples.join(","))
    } else {
        format!("[{}× {}]", slot.distinct, slot.samples.join(","))
    }
}

/// Render a single output line.
fn render_line(line: &OutputLine) -> String {
    match line {
        OutputLine::Kept {
            text, timestamp, ..
        } => match timestamp {
            Some(ts) => format!("[{ts}] {text}"),
            None => text.clone(),
        },
        OutputLine::Collapsed {
            count,
            template,
            slots,
            ..
        } => {
            let mut s = format!("... [x{count}] {template}");
            for slot in slots {
                s.push(' ');
                s.push_str(&fmt_slot(slot));
            }
            s
        }
    }
}

// ---------------------------------------------------------------------------
// compress()
// ---------------------------------------------------------------------------

/// True if a line is a "signal" line (severity level OR a consequence/context
/// shape per the role overrides).
fn is_signal(line: &LogLine, signal_levels: &[String]) -> bool {
    if let Some(lvl) = &line.level {
        if signal_levels.iter().any(|s| s == &lvl.to_lowercase()) {
            return true;
        }
    }
    matches!(
        RoleClassifier::override_role(&line.message, None),
        Role::Consequence | Role::Context
    )
}

/// Clip a template to `template_clip` chars (rendered group header).
fn clip(template: &str, max: usize) -> String {
    if template.chars().count() <= max {
        template.to_string()
    } else {
        template.chars().take(max).collect()
    }
}

/// Build a `SlotStat` for slot `si` of group `g_idx`.
fn build_slot_stat(profile: &Profile, g_idx: usize, si: usize) -> SlotStat {
    let prof = &profile.profiles[g_idx];
    // Collect this slot's captured raw values across members.
    let values: Vec<Option<String>> = prof
        .raw_slots
        .iter()
        .map(|m| m.get(si).cloned().flatten())
        .collect();
    let (samples, distinct) = distinct_samples(&values, SAMPLE_CAP);
    let unit = samples
        .first()
        .map(|s| trailing_alpha_unit(s))
        .unwrap_or_default();

    match &prof.numeric[si] {
        Some(num) => SlotStat {
            numeric: true,
            min: num.min,
            max: num.max,
            median: num.median,
            unit,
            distinct,
            samples,
        },
        None => SlotStat {
            numeric: false,
            min: 0.0,
            max: 0.0,
            median: 0.0,
            unit,
            distinct,
            samples,
        },
    }
}

/// Trailing-alphabetic unit of a value (e.g. "ms" from "45ms", "MB" from "512MB").
/// Empty if the value does not end in letters or is purely alphabetic.
fn trailing_alpha_unit(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut i = chars.len();
    while i > 0 && chars[i - 1].is_ascii_alphabetic() {
        i -= 1;
    }
    // Require at least one preceding non-alpha char so a pure word isn't a "unit".
    if i == chars.len() || i == 0 {
        return String::new();
    }
    chars[i..].iter().collect()
}

/// Compress `lines` per the NEW keep-policy.
pub fn compress(lines: &[LogLine], config: &CompressorConfig) -> CompressionResult {
    let norm = Normalizer::new();
    let guard = Guard::new();
    let original_count = lines.len();

    if lines.is_empty() {
        return CompressionResult {
            lines: Vec::new(),
            original_count: 0,
            kept_count: 0,
        };
    }

    // 1. Group.
    let grouper = make_grouper(config.grouper, config.prefix_len, config.jaccard_th);
    let groups = grouper.group(lines, &norm, &guard);

    // 2. Profile.
    let profile = Profile::build(lines, &groups, &norm, config.outlier_factor);
    let gid = &profile.gid;

    // 3. Keep set.
    let n = lines.len();
    let mut kept = vec![false; n];

    // 3a. Signal cap: keep up to signal_sample_cap signal lines per group.
    let mut signal_kept_per_group: Vec<usize> = vec![0; groups.len()];
    for (i, line) in lines.iter().enumerate() {
        if is_signal(line, &config.signal_levels) {
            let g = gid[i];
            if signal_kept_per_group[g] < config.signal_sample_cap {
                kept[i] = true;
                signal_kept_per_group[g] += 1;
            }
        }
    }

    // 3b. Outlier rescue: keep any line with a tail-outlier numeric slot.
    for (g_idx, g) in groups.iter().enumerate() {
        let prof = &profile.profiles[g_idx];
        for (pos, &line_idx) in g.member_indices.iter().enumerate() {
            if kept[line_idx] {
                continue;
            }
            for si in 0..prof.slot_count {
                if let Some(raw) = prof.raw_slots[pos].get(si).and_then(|o| o.as_ref()) {
                    if let Some(v) = Profile::slot_numeric_value(raw) {
                        if profile.is_tail_outlier(g_idx, si, v) {
                            kept[line_idx] = true;
                            break;
                        }
                    }
                }
            }
        }
    }

    // 3c. Collapse decision per group.
    // collapsing[g] = true => emit one Collapsed for its hidden members.
    // otherwise its hidden members are rare candidates.
    let mut collapsing = vec![false; groups.len()];
    let mut rare_candidates: Vec<usize> = Vec::new();
    for (g_idx, g) in groups.iter().enumerate() {
        let hidden: Vec<usize> = g
            .member_indices
            .iter()
            .copied()
            .filter(|&i| !kept[i])
            .collect();
        let template = &profile.profiles[g_idx].template;
        let useful = template::is_useful_template(template, config.min_static_chars);
        if useful && hidden.len() >= MIN_COLLAPSE {
            collapsing[g_idx] = true;
        } else {
            rare_candidates.extend(hidden);
        }
    }

    // 3d. Rare-tail budget: keep first max_rare_lines rare candidates verbatim;
    // the rest are dropped into a single tail-summary.
    rare_candidates.sort_unstable();
    let mut dropped: Vec<usize> = Vec::new();
    for (rank, &idx) in rare_candidates.iter().enumerate() {
        if rank < config.max_rare_lines {
            kept[idx] = true;
        } else {
            dropped.push(idx);
        }
    }

    // Distinct shapes among the dropped lines (for the tail summary message).
    let dropped_shapes: std::collections::BTreeSet<usize> =
        dropped.iter().map(|&i| gid[i]).collect();

    let kept_count = kept.iter().filter(|&&k| k).count();

    // 4 + 5. Emit chronologically. A line is one of: kept verbatim; the first
    // hidden member of a collapsing group (emits one Collapsed); or a
    // collapsed/dropped member that is skipped (dropped lines fall through to
    // the tail summary below).
    let mut out: Vec<OutputLine> = Vec::new();
    let mut group_emitted = vec![false; groups.len()];

    for (i, line) in lines.iter().enumerate() {
        let g = gid[i];
        if kept[i] {
            out.push(OutputLine::Kept {
                index: i,
                text: line.message.clone(),
                timestamp: line.timestamp.clone(),
            });
        } else if collapsing[g] && !group_emitted[g] {
            group_emitted[g] = true;
            let hidden_count = groups[g]
                .member_indices
                .iter()
                .filter(|&&m| !kept[m])
                .count();
            let prof = &profile.profiles[g];
            let slots: Vec<SlotStat> = (0..prof.slot_count)
                .map(|si| build_slot_stat(&profile, g, si))
                .collect();
            out.push(OutputLine::Collapsed {
                first_index: i,
                count: hidden_count,
                template: clip(&prof.template, config.template_clip),
                slots,
                timestamp: line.timestamp.clone(),
            });
        }
        // Other collapsed members and dropped members are skipped here.
    }

    // Tail-summary entry for dropped one-off lines.
    if !dropped.is_empty() {
        out.push(OutputLine::Collapsed {
            first_index: *dropped.iter().min().unwrap(),
            count: dropped.len(),
            template: format!(
                "<{} one-off lines, {} shapes>",
                dropped.len(),
                dropped_shapes.len()
            ),
            slots: Vec::new(),
            timestamp: None,
        });
    }

    CompressionResult {
        lines: out,
        original_count,
        kept_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(msg: &str) -> LogLine {
        LogLine::new(msg.to_string())
    }

    fn leveled(msg: &str, level: &str) -> LogLine {
        LogLine {
            message: msg.to_string(),
            level: Some(level.to_string()),
            timestamp: None,
        }
    }

    /// Build the ~40-line db-pool cascade fixture.
    fn db_pool_cascade() -> Vec<LogLine> {
        let mut v = Vec::new();
        // Long routine run: 30 "acquired connection from pool, in_use=N" lines.
        for k in 0..30 {
            v.push(line(&format!(
                "acquired connection from pool, in_use={}",
                10 + (k % 5)
            )));
        }
        // A value spike (outlier) inside the same template.
        v.push(line("acquired connection from pool, in_use=9000"));
        // A few more routine lines after the spike.
        for k in 0..5 {
            v.push(line(&format!(
                "acquired connection from pool, in_use={}",
                12 + (k % 3)
            )));
        }
        // Cascade tail.
        v.push(leveled("db connection pool saturated at 95%", "warn"));
        v.push(leveled("db_pool exhausted waiting=12", "error"));
        v.push(leveled(
            r#"10.0.0.1 - - "GET /checkout HTTP/1.1" 503 0"#,
            "error",
        ));
        v.push(leveled("query timeout after 30s on /checkout", "error"));
        v.push(leveled("circuit breaker payments OPEN", "fatal"));
        v
    }

    fn cascade_messages() -> Vec<&'static str> {
        vec![
            "db connection pool saturated at 95%",
            "db_pool exhausted waiting=12",
            r#"10.0.0.1 - - "GET /checkout HTTP/1.1" 503 0"#,
            "query timeout after 30s on /checkout",
            "circuit breaker payments OPEN",
        ]
    }

    fn rendered_contains(result: &CompressionResult, needle: &str) -> bool {
        result.render().contains(needle)
    }

    #[test]
    fn golden_db_pool_cascade_lossless() {
        let lines = db_pool_cascade();
        let cfg = CompressorConfig::for_mode(Mode::Lossless);
        let result = compress(&lines, &cfg);

        // All cascade lines kept verbatim.
        for c in cascade_messages() {
            assert!(rendered_contains(&result, c), "missing cascade line: {c}");
        }
        // The value spike kept (it is a signal-free line but an outlier).
        assert!(rendered_contains(&result, "in_use=9000"));
        // Routine collapsed to a value-summary line.
        assert!(rendered_contains(&result, "acquired connection from pool"));
        assert!(result.render().contains("[x"));
        // kept < original.
        assert!(result.kept_count < result.original_count);
        // Output index-ordered.
        assert_index_ordered(&result);
    }

    #[test]
    fn golden_db_pool_cascade_balanced() {
        let lines = db_pool_cascade();
        let cfg = CompressorConfig::for_mode(Mode::Balanced);
        let result = compress(&lines, &cfg);

        for c in cascade_messages() {
            assert!(rendered_contains(&result, c), "missing cascade line: {c}");
        }
        assert!(rendered_contains(&result, "in_use=9000"));
        assert!(rendered_contains(&result, "acquired connection from pool"));
        assert!(result.kept_count < result.original_count);
        assert_index_ordered(&result);
    }

    fn assert_index_ordered(result: &CompressionResult) {
        let mut last = -1i64;
        for l in &result.lines {
            let idx = match l {
                OutputLine::Kept { index, .. } => *index as i64,
                OutputLine::Collapsed { first_index, .. } => *first_index as i64,
            };
            assert!(idx >= last, "output not index-ordered: {idx} after {last}");
            last = idx;
        }
    }

    #[test]
    fn determinism_identical_render() {
        let lines = db_pool_cascade();
        let cfg = CompressorConfig::default();
        let a = compress(&lines, &cfg).render();
        let b = compress(&lines, &cfg).render();
        assert_eq!(a, b);
    }

    #[test]
    fn lossless_keeps_at_least_as_many_as_balanced() {
        let lines = db_pool_cascade();
        let lossless = compress(&lines, &CompressorConfig::for_mode(Mode::Lossless));
        let balanced = compress(&lines, &CompressorConfig::for_mode(Mode::Balanced));
        assert!(lossless.kept_count >= balanced.kept_count);
        // Balanced still keeps every signal/outlier line.
        for c in cascade_messages() {
            assert!(balanced.render().contains(c));
        }
        assert!(balanced.render().contains("in_use=9000"));
    }

    #[test]
    fn signal_sample_cap_caps_per_group() {
        // 10 warn lines of the SAME template -> Aggressive cap=2 keeps 2 signals.
        let mut v = Vec::new();
        for k in 0..10 {
            v.push(leveled(&format!("disk pressure warning level {k}"), "warn"));
        }
        let cfg = CompressorConfig::for_mode(Mode::Aggressive);
        let result = compress(&v, &cfg);
        let kept_signals = result
            .lines
            .iter()
            .filter(|l| matches!(l, OutputLine::Kept { .. }))
            .count();
        // cap is 2 signal lines per group (rest collapse).
        assert!(kept_signals <= 2, "kept {kept_signals} > cap 2");
        assert!(result.render().contains("[x"));
    }

    #[test]
    fn max_rare_lines_budget_drops_to_tail() {
        // Many DISTINCT one-off shapes (each its own group, none collapsible)
        // exceeding max_rare_lines -> overflow goes to a tail summary. Each line
        // carries a distinct STATIC word so the structural skeletons differ and
        // every group has size 1 (below MIN_COLLAPSE).
        let words = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
            "hotel", "india", "juliet", "kilo", "lima", "mike", "november",
            "oscar", "papa", "quebec", "romeo", "sierra", "tango", "uniform",
            "victor", "whiskey", "xray", "yankee", "zulu", "apple", "banana",
            "cherry", "date",
        ];
        let mut v = Vec::new();
        for (k, w) in words.iter().take(30).enumerate() {
            // Vary the static-word *count* per line too so the adaptive merge
            // can't fold them (length-delta + distinct anchors keep them apart),
            // and pin the Structural grouper to guarantee one group per shape.
            let pad = "x".repeat(k % 3); // tiny static variation
            v.push(line(&format!("unique startup probe {w}{pad} ready done")));
        }
        // max_rare_lines = 20 (aggressive budget) with the Structural grouper so
        // each distinct shape stays its own size-1 group (below MIN_COLLAPSE) ->
        // all 30 are rare candidates; 30 - 20 = 10 overflow to the tail summary.
        let mut cfg = CompressorConfig::for_mode(Mode::Aggressive);
        cfg.grouper = GrouperKind::Structural;
        let result = compress(&v, &cfg);
        // There must be a tail summary entry.
        let tail = result
            .lines
            .iter()
            .find(|l| matches!(l, OutputLine::Collapsed { template, .. } if template.contains("one-off")));
        assert!(tail.is_some(), "expected a one-off tail summary");
        if let Some(OutputLine::Collapsed { count, .. }) = tail {
            assert_eq!(*count, 10, "30 lines - 20 budget = 10 dropped");
        }
    }

    #[test]
    fn empty_input() {
        let result = compress(&[], &CompressorConfig::default());
        assert_eq!(result.original_count, 0);
        assert_eq!(result.kept_count, 0);
        assert_eq!(result.lines.len(), 0);
        assert_eq!(result.render(), "");
    }

    #[test]
    fn fmt_num_no_trailing_zero() {
        assert_eq!(fmt_num(20.0), "20");
        assert_eq!(fmt_num(0.8), "0.8");
        assert_eq!(fmt_num(2.50), "2.5");
    }

    #[test]
    fn trailing_unit_extraction() {
        assert_eq!(trailing_alpha_unit("45ms"), "ms");
        assert_eq!(trailing_alpha_unit("512MB"), "MB");
        assert_eq!(trailing_alpha_unit("100"), "");
        assert_eq!(trailing_alpha_unit("word"), "");
    }
}
