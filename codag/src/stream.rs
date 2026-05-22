//! Streaming index (Phase 2).
//!
//! [`StreamingIndex`] ingests log lines one-at-a-time and projects a
//! [`CompressionResult`] (the same shape the batch [`crate::compress`] produces)
//! on demand. It is the online counterpart of the batch compressor.
//!
//! ## Scope: structural-only, lossless grouping
//!
//! Streaming uses the **structural** grouper exclusively. The structural
//! skeleton ([`Normalizer::structural_normalize_for_grouping`]) is an
//! O(1)-per-line exact key, which makes online grouping trivial and exact: a
//! line's group is decided the moment it arrives, never revised. The
//! [`crate::compress::GrouperKind::Adaptive`] de-fragmentation pass (a global
//! O(n²) union-find over skeletons) is a **batch-only** extra and is
//! intentionally out of scope here — it would require revisiting earlier group
//! assignments, which the streaming contract forbids.
//!
//! ## Parity contract
//!
//! For the structural grouper, the index is **byte-for-byte identical** to the
//! batch path:
//!
//! ```text
//! StreamingIndex::new(cfg).push(all lines).capsule().render()
//!     == compress(&lines, &cfg).render()      // cfg.grouper == Structural
//! ```
//!
//! This holds because `capsule()` assembles the group `Vec` in exactly the
//! order [`crate::compress::grouper::StructuralExactGrouper`] would produce
//! (groups by ascending first-member index, members ascending) and then calls
//! the shared [`compress_groups`] — the identical post-grouping pipeline the
//! batch path uses.
//!
//! Value statistics (min/max/median, outlier rescue) are computed by
//! `Profile::build` over the accumulated lines at `capsule()` time, so they are
//! **exact** — no streaming approximation. `push()` stays O(1) amortized for
//! grouping; `capsule()` is an O(n) on-demand projection (acceptable for the
//! daemon). A bounded / incremental-stats optimization is an explicit
//! follow-up, not Phase 2.

use std::collections::HashMap;

use crate::compress::grouper::Group;
use crate::compress::normalize::Normalizer;
use crate::compress::{compress_groups, CompressionResult, CompressorConfig, LogLine};

/// Online structural log index.
///
/// Accumulates lines and their structural-group assignment incrementally, then
/// projects a [`CompressionResult`] on demand via [`StreamingIndex::capsule`].
#[derive(Debug, Clone)]
pub struct StreamingIndex {
    config: CompressorConfig,
    norm: Normalizer,
    /// Accumulated lines, in arrival (= ascending index) order.
    lines: Vec<LogLine>,
    /// Structural skeleton (joined key) -> group slot index in `groups`.
    skeleton_to_group: HashMap<String, usize>,
    /// `groups[g]` = ascending member line indices, groups in first-seen order.
    groups: Vec<Vec<usize>>,
}

impl StreamingIndex {
    /// Create an empty index. The `config`'s grouper field is ignored —
    /// streaming always groups structurally (see the module docs) — but the
    /// rest of the config (modes, caps, budgets, outlier factor) is honored,
    /// exactly as the batch path would honor it.
    pub fn new(config: CompressorConfig) -> Self {
        StreamingIndex {
            config,
            norm: Normalizer::new(),
            lines: Vec::new(),
            skeleton_to_group: HashMap::new(),
            groups: Vec::new(),
        }
    }

    /// Ingest one line. O(1) amortized: compute its structural skeleton, look up
    /// (or create) the matching group slot, and append the new line index.
    pub fn push(&mut self, line: LogLine) {
        let idx = self.lines.len();
        // Same skeleton fn the batch StructuralExactGrouper keys on. We join the
        // token vector with `\u{0}` (a byte that cannot appear in a whitespace
        // token) into a single hashable key — equivalent to keying on the
        // `Vec<String>` itself.
        let skeleton = self.norm.structural_normalize_for_grouping(&line.message);
        let key = skeleton.join("\u{0}");
        let slot = match self.skeleton_to_group.get(&key) {
            Some(&g) => g,
            None => {
                let g = self.groups.len();
                self.groups.push(Vec::new());
                self.skeleton_to_group.insert(key, g);
                g
            }
        };
        // Lines arrive in ascending index order, so members stay ascending.
        self.groups[slot].push(idx);
        self.lines.push(line);
    }

    /// Ingest many lines (convenience for `push` in a loop).
    pub fn extend<I: IntoIterator<Item = LogLine>>(&mut self, lines: I) {
        for line in lines {
            self.push(line);
        }
    }

    /// Number of lines ingested so far.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// True if no lines have been ingested.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Assemble the canonical group `Vec` (identical ordering to
    /// `StructuralExactGrouper::group` on the same lines) and project a
    /// [`CompressionResult`] via the shared post-grouping pipeline, using the
    /// index's stored [`CompressorConfig`].
    pub fn capsule(&self) -> CompressionResult {
        self.capsule_with(&self.config)
    }

    /// Project a [`CompressionResult`] using a **per-request** `config` instead
    /// of the index's stored one.
    ///
    /// Grouping is structural-only and already settled by `push`, so this just
    /// re-runs the shared post-grouping pipeline ([`compress_groups`]) with the
    /// given config — letting one streamed session be projected at different
    /// modes/budgets (lossless / balanced / aggressive) on demand. The `config`'s
    /// `grouper` field is ignored (streaming is always structural), exactly as
    /// [`StreamingIndex::new`] documents.
    pub fn capsule_with(&self, config: &CompressorConfig) -> CompressionResult {
        if self.lines.is_empty() {
            return CompressionResult {
                lines: Vec::new(),
                original_count: 0,
                kept_count: 0,
            };
        }
        let groups = self.finalize_groups();
        compress_groups(&self.lines, &groups, config)
    }

    /// Reproduce `grouper::finalize`: members ascending (already true by
    /// construction, sort defensively), groups ordered by smallest member index.
    /// For the structural grouper this is identical to first-seen group order,
    /// but we sort to match the batch path's emitted ordering exactly.
    fn finalize_groups(&self) -> Vec<Group> {
        let mut buckets: Vec<Vec<usize>> = self.groups.clone();
        for b in &mut buckets {
            b.sort_unstable();
        }
        buckets.sort_by_key(|b| b.first().copied().unwrap_or(usize::MAX));
        buckets
            .into_iter()
            .filter(|b| !b.is_empty())
            .map(|member_indices| Group { member_indices })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::{compress, GrouperKind, Mode};

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

    /// The golden db-pool cascade (mirrors compress.rs's fixture).
    fn db_pool_cascade() -> Vec<LogLine> {
        let mut v = Vec::new();
        for k in 0..30 {
            v.push(line(&format!(
                "acquired connection from pool, in_use={}",
                10 + (k % 5)
            )));
        }
        v.push(line("acquired connection from pool, in_use=9000"));
        for k in 0..5 {
            v.push(line(&format!(
                "acquired connection from pool, in_use={}",
                12 + (k % 3)
            )));
        }
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

    /// A diverse multi-template log with several shapes, repeats, and an outlier.
    fn diverse_multi_template() -> Vec<LogLine> {
        let mut v = Vec::new();
        for k in 0..8 {
            v.push(line(&format!("GET /api/users/{k} 200 ok")));
        }
        for k in 0..6 {
            v.push(line(&format!("cache hit key=session:{k} latency=2ms")));
        }
        v.push(line("cache hit key=session:99 latency=9000ms")); // outlier
        for k in 0..4 {
            v.push(leveled(
                &format!("retry attempt {k} for upstream svc-{k}"),
                "warn",
            ));
        }
        v.push(leveled("upstream svc-payments unreachable", "error"));
        v.push(leveled("circuit breaker payments OPEN", "fatal"));
        v.push(line("worker idle, queue depth=3"));
        v.push(line("worker idle, queue depth=4"));
        v.push(line("worker idle, queue depth=5"));
        v.push(line("config reloaded from /etc/app/config.yaml"));
        v
    }

    fn cfg_structural(mode: Mode) -> CompressorConfig {
        let mut c = CompressorConfig::for_mode(mode);
        c.grouper = GrouperKind::Structural;
        c
    }

    fn pushed(lines: &[LogLine], cfg: &CompressorConfig) -> StreamingIndex {
        let mut idx = StreamingIndex::new(cfg.clone());
        for l in lines {
            idx.push(l.clone());
        }
        idx
    }

    #[test]
    fn len_and_is_empty() {
        let cfg = cfg_structural(Mode::Lossless);
        let mut idx = StreamingIndex::new(cfg);
        assert!(idx.is_empty());
        assert_eq!(idx.len(), 0);
        idx.push(line("hello world"));
        idx.push(line("hello world"));
        assert!(!idx.is_empty());
        assert_eq!(idx.len(), 2);
    }

    #[test]
    fn empty_capsule() {
        let idx = StreamingIndex::new(cfg_structural(Mode::Balanced));
        let r = idx.capsule();
        assert_eq!(r.original_count, 0);
        assert_eq!(r.kept_count, 0);
        assert!(r.lines.is_empty());
        assert_eq!(r.render(), "");
    }

    #[test]
    fn single_group_collapses() {
        // Many identical-shape lines -> one structural group -> collapses.
        let mut lines = Vec::new();
        for k in 0..12 {
            lines.push(line(&format!("processed batch item {}", k % 4)));
        }
        let cfg = cfg_structural(Mode::Lossless);
        let idx = pushed(&lines, &cfg);
        let r = idx.capsule();
        assert_eq!(r.original_count, 12);
        assert!(r.render().contains("[x"), "expected a collapsed group");
    }

    #[test]
    fn multi_group_distinct_shapes() {
        let lines = vec![
            line("acquired connection from pool"),
            line("released connection to pool"),
            line("acquired connection from pool"),
            line("released connection to pool"),
        ];
        let cfg = cfg_structural(Mode::Lossless);
        let idx = pushed(&lines, &cfg);
        // Two distinct skeletons => two groups.
        assert_eq!(idx.finalize_groups().len(), 2);
    }

    // ----- The key gate: EXACT byte-for-byte parity with batch compress() -----

    fn assert_parity(lines: &[LogLine], mode: Mode) {
        let cfg = cfg_structural(mode);
        let streamed = pushed(lines, &cfg).capsule().render();
        let batch = compress(lines, &cfg).render();
        assert_eq!(
            streamed, batch,
            "streaming != batch (mode {mode:?})\n--- streamed ---\n{streamed}\n--- batch ---\n{batch}"
        );
    }

    #[test]
    fn parity_db_pool_cascade_all_modes() {
        let lines = db_pool_cascade();
        assert_parity(&lines, Mode::Lossless);
        assert_parity(&lines, Mode::Balanced);
        assert_parity(&lines, Mode::Aggressive);
    }

    #[test]
    fn parity_diverse_multi_template_all_modes() {
        let lines = diverse_multi_template();
        assert_parity(&lines, Mode::Lossless);
        assert_parity(&lines, Mode::Balanced);
        assert_parity(&lines, Mode::Aggressive);
    }

    #[test]
    fn parity_many_distinct_one_offs() {
        // Exercises the rare-tail budget / one-off tail summary path.
        let words = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
            "juliet", "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo",
            "sierra", "tango", "uniform", "victor", "whiskey", "xray", "yankee", "zulu", "apple",
            "banana", "cherry", "date",
        ];
        let mut v = Vec::new();
        for (k, w) in words.iter().take(30).enumerate() {
            let pad = "x".repeat(k % 3);
            v.push(line(&format!("unique startup probe {w}{pad} ready done")));
        }
        assert_parity(&v, Mode::Aggressive);
        assert_parity(&v, Mode::Balanced);
        assert_parity(&v, Mode::Lossless);
    }

    #[test]
    fn parity_single_line() {
        assert_parity(&[line("just one line here")], Mode::Balanced);
    }

    #[test]
    fn parity_render_byte_for_byte_exact() {
        // Spell out the contract verbatim against both golden inputs, Balanced.
        let cfg = cfg_structural(Mode::Balanced);
        for lines in [db_pool_cascade(), diverse_multi_template()] {
            let streamed = pushed(&lines, &cfg).capsule();
            let batch = compress(&lines, &cfg);
            assert_eq!(streamed.render(), batch.render());
            assert_eq!(streamed.original_count, batch.original_count);
            assert_eq!(streamed.kept_count, batch.kept_count);
            assert_eq!(streamed.lines.len(), batch.lines.len());
            // Structural equality of the OutputLine vectors, not just render.
            assert_eq!(streamed.lines, batch.lines);
        }
    }

    // ----- Determinism: two independent push-sequences -> identical capsule. -----

    #[test]
    fn determinism_two_independent_sequences() {
        let lines = db_pool_cascade();
        let cfg = cfg_structural(Mode::Balanced);
        let a = pushed(&lines, &cfg).capsule().render();
        let b = pushed(&lines, &cfg).capsule().render();
        assert_eq!(a, b);
    }

    #[test]
    fn determinism_diverse() {
        let lines = diverse_multi_template();
        let cfg = cfg_structural(Mode::Aggressive);
        let a = pushed(&lines, &cfg).capsule();
        let b = pushed(&lines, &cfg).capsule();
        assert_eq!(a.render(), b.render());
        assert_eq!(a.lines, b.lines);
    }

    // ----- capsule_with(per-request config) parity. -----

    #[test]
    fn capsule_with_per_request_config_matches_batch() {
        // An index built with one config, projected with a *different* per-request
        // config, must equal a batch structural compress at that config.
        let lines = db_pool_cascade();
        // Build the index at Lossless, then project at Balanced via capsule_with.
        let idx = pushed(&lines, &cfg_structural(Mode::Lossless));
        let balanced = cfg_structural(Mode::Balanced);
        let projected = idx.capsule_with(&balanced);
        let batch = compress(&lines, &balanced);
        assert_eq!(projected.render(), batch.render());
        assert_eq!(projected.lines, batch.lines);
        assert_eq!(projected.original_count, batch.original_count);
        assert_eq!(projected.kept_count, batch.kept_count);
    }

    #[test]
    fn capsule_with_all_modes_diverse() {
        let lines = diverse_multi_template();
        let idx = pushed(&lines, &cfg_structural(Mode::Lossless));
        for mode in [Mode::Lossless, Mode::Balanced, Mode::Aggressive] {
            let cfg = cfg_structural(mode);
            assert_eq!(
                idx.capsule_with(&cfg).render(),
                compress(&lines, &cfg).render(),
                "capsule_with mismatch at {mode:?}"
            );
        }
    }

    #[test]
    fn capsule_equals_capsule_with_stored_config() {
        let lines = diverse_multi_template();
        let cfg = cfg_structural(Mode::Aggressive);
        let idx = pushed(&lines, &cfg);
        assert_eq!(idx.capsule().render(), idx.capsule_with(&cfg).render());
    }

    #[test]
    fn capsule_stable_across_repeated_calls() {
        let lines = diverse_multi_template();
        let cfg = cfg_structural(Mode::Balanced);
        let idx = pushed(&lines, &cfg);
        let first = idx.capsule().render();
        let second = idx.capsule().render();
        assert_eq!(first, second, "capsule() must be a pure projection");
    }
}
