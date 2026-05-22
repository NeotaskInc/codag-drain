//! Anti-overmerge guard.
//!
//! Port of `_has_bad_static_conflict` from `v2/src/v2/templater/grouping.py`
//! (`has_bad_static_conflict`) plus a relaxed variant `has_semantic_conflict`
//! used by the adaptive post-merge step: it drops the generic-alpha conflict
//! (`len>=3 alpha vs alpha`) so near-identical skeletons (e.g. worker names)
//! can still merge, but keeps the hard antonym / HTTP-method / status conflicts.

use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

const ANTONYM_GROUPS: &[&[&str]] = &[
    &["success", "succeeded", "successful", "ok", "healthy", "ready"],
    &["fail", "failed", "failure", "error", "errored", "unhealthy"],
    &["open", "opened", "opening"],
    &["close", "closed", "closing"],
    &["start", "started", "starting"],
    &["stop", "stopped", "stopping"],
    &["accept", "accepted", "allow", "allowed"],
    &["reject", "rejected", "deny", "denied"],
    &["enable", "enabled"],
    &["disable", "disabled"],
    &["connect", "connected"],
    &["disconnect", "disconnected"],
];

const HTTP_METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "head", "options"];

fn antonym_index() -> &'static HashMap<&'static str, usize> {
    static M: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();
    M.get_or_init(|| {
        let mut m = HashMap::new();
        for (idx, words) in ANTONYM_GROUPS.iter().enumerate() {
            for &w in *words {
                m.insert(w, idx);
            }
        }
        m
    })
}

fn var_re() -> &'static Regex {
    // _VAR_RE = r"<(?:LEVEL|NUM|HTTP_[1-5]XX|VAR|ID|ADDR|STR|VAL)>", IGNORECASE
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)<(?:LEVEL|NUM|HTTP_[1-5]XX|VAR|ID|ADDR|STR|VAL)>").unwrap()
    })
}

fn strip_punct(s: &str) -> &str {
    s.trim_matches(|c| "[](){}<>.,;:'\"".contains(c))
}

/// The guard. `relaxed=false` reproduces `_has_bad_static_conflict` exactly.
/// `relaxed=true` is the adaptive-grouper variant: identical, except the final
/// generic-alpha conflict (two distinct alpha tokens of len>=3) is *not* counted,
/// so only antonyms / HTTP-method / HTTP-status / length-delta conflicts block.
#[derive(Debug, Default, Clone)]
pub struct Guard;

impl Guard {
    pub fn new() -> Self {
        Guard
    }

    /// Exact port of `_has_bad_static_conflict(a, b)`.
    pub fn has_bad_static_conflict(&self, a: &[String], b: &[String]) -> bool {
        self.conflict(a, b, false)
    }

    /// Relaxed variant for the adaptive post-merge: antonyms / HTTP-method /
    /// status / length-delta only (NOT generic-alpha mismatch).
    pub fn has_semantic_conflict(&self, a: &[String], b: &[String]) -> bool {
        self.conflict(a, b, true)
    }

    fn conflict(&self, a: &[String], b: &[String], relaxed: bool) -> bool {
        if (a.len() as i64 - b.len() as i64).abs() > 2 {
            return true;
        }
        let min_len = a.len().min(b.len());
        let mut conflicts = 0usize;
        let idx = antonym_index();
        for i in 0..min_len {
            let left_full = a[i].to_lowercase();
            let right_full = b[i].to_lowercase();
            if left_full == right_full {
                continue;
            }
            if left_full.starts_with("<http_") && right_full.starts_with("<http_") {
                return true;
            }
            if var_re().is_match(&left_full) || var_re().is_match(&right_full) {
                continue;
            }
            let left = strip_punct(&left_full);
            let right = strip_punct(&right_full);
            let li = idx.get(left);
            let ri = idx.get(right);
            if let (Some(&li), Some(&ri)) = (li, ri) {
                if li != ri {
                    return true;
                }
            }
            if HTTP_METHODS.contains(&left) && HTTP_METHODS.contains(&right) {
                return true;
            }
            if !relaxed
                && left.chars().any(|c| c.is_alphabetic())
                && right.chars().any(|c| c.is_alphabetic())
                && left.len() >= 3
                && right.len() >= 3
            {
                conflicts += 1;
            }
        }
        conflicts >= 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn antonym_blocks() {
        let g = Guard::new();
        assert!(g.has_bad_static_conflict(&v(&["task", "succeeded"]), &v(&["task", "failed"])));
        // relaxed still blocks antonyms
        assert!(g.has_semantic_conflict(&v(&["task", "succeeded"]), &v(&["task", "failed"])));
    }

    #[test]
    fn http_method_blocks() {
        let g = Guard::new();
        assert!(g.has_bad_static_conflict(&v(&["get", "/x"]), &v(&["post", "/x"])));
        assert!(g.has_semantic_conflict(&v(&["get", "/x"]), &v(&["post", "/x"])));
    }

    #[test]
    fn http_status_2xx_vs_5xx_blocks() {
        let g = Guard::new();
        assert!(g.has_bad_static_conflict(&v(&["got", "<HTTP_2XX>"]), &v(&["got", "<HTTP_5XX>"])));
        assert!(g.has_semantic_conflict(&v(&["got", "<HTTP_2XX>"]), &v(&["got", "<HTTP_5XX>"])));
    }

    #[test]
    fn length_delta_blocks() {
        let g = Guard::new();
        assert!(g.has_bad_static_conflict(&v(&["a"]), &v(&["a", "b", "c", "d"])));
        assert!(g.has_semantic_conflict(&v(&["a"]), &v(&["a", "b", "c", "d"])));
    }

    #[test]
    fn numeric_only_diff_does_not_conflict() {
        let g = Guard::new();
        // <NUM> vs <NUM> equal -> no conflict; var vs var skipped.
        assert!(!g.has_bad_static_conflict(&v(&["pool", "<NUM>"]), &v(&["pool", "<NUM>"])));
    }

    #[test]
    fn generic_alpha_conflicts_only_when_strict() {
        let g = Guard::new();
        let a = v(&["worker", "alpha"]);
        let b = v(&["worker", "bravo"]);
        // strict: two distinct alpha tokens len>=3 -> conflict
        assert!(g.has_bad_static_conflict(&a, &b));
        // relaxed: generic-alpha mismatch ignored -> no conflict
        assert!(!g.has_semantic_conflict(&a, &b));
    }
}
