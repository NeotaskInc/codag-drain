//! Template derivation + regex compilation.
//!
//! The grouping step is Drain-style and data-driven. This module only turns an
//! already-formed group into a conservative `<*>` template and a capture regex
//! for slot summaries.
//!
//! DIVERGENCE: the Python `derive_pair_template` uses `difflib.SequenceMatcher`.
//! We implement an in-crate LCS `get_opcodes` (classic DP over the two token
//! sequences, emitting `equal` runs and `gap` regions). This can differ from
//! difflib's heuristic alignment on some inputs, but it is SAFE here because
//! every derived template is re-validated downstream by
//! `regex_from_placeholder` actually matching the group members AND by
//! `is_useful_template`; a worse alignment only ever produces a template that
//! fails one of those checks (and is then discarded / falls back), never a
//! wrong-but-accepted one.

use regex::Regex;

pub const PLACEHOLDER: &str = "<*>";
const REGEX_VAR: &str = "(.*?)";
const ALLOWED_NOISE_PUNCT: &str = r##"!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~"##;

fn template_token(raw: &str, normalized: &str) -> String {
    if normalized == PLACEHOLDER {
        PLACEHOLDER.to_string()
    } else {
        raw.to_string()
    }
}

fn tokenize(line: &str) -> Vec<String> {
    line.split_whitespace().map(|s| s.to_string()).collect()
}

/// Port of `_collapse_placeholders`: drop a `<*>` that immediately follows
/// another `<*>`.
pub fn collapse_placeholders(tokens: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    for tok in tokens {
        if tok == PLACEHOLDER && out.last().map(|s| s.as_str()) == Some(PLACEHOLDER) {
            continue;
        }
        out.push(tok.clone());
    }
    out
}

// ---------------------------------------------------------------------------
// In-crate LCS opcodes (replaces difflib.SequenceMatcher.get_opcodes).
// ---------------------------------------------------------------------------

/// An opcode tag for the alignment of two token sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpTag {
    /// Equal run: a[i1..i2] == b[j1..j2] token-for-token.
    Equal,
    /// Any non-equal region (replace/insert/delete combined into one "gap").
    Gap,
}

/// One aligned block. `(tag, i1, i2, j1, j2)` mirrors difflib's tuple shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpCode {
    pub tag: OpTag,
    pub i1: usize,
    pub i2: usize,
    pub j1: usize,
    pub j2: usize,
}

/// Compute alignment opcodes via classic LCS DP, then merge non-equal regions
/// into single `Gap` blocks (so consecutive replace/insert/delete collapse).
pub fn get_opcodes(a: &[String], b: &[String]) -> Vec<OpCode> {
    let n = a.len();
    let m = b.len();
    // LCS length DP table (n+1) x (m+1).
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    // Backtrack to produce a sequence of matched index pairs.
    let mut matches: Vec<(usize, usize)> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            matches.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }

    // Walk matches, emitting Gap for unmatched spans and merging contiguous
    // equal pairs into single Equal runs.
    let mut ops: Vec<OpCode> = Vec::new();
    let (mut ai, mut bj) = (0usize, 0usize);
    let mut k = 0usize;
    while k < matches.len() {
        let (mi, mj) = matches[k];
        if ai < mi || bj < mj {
            ops.push(OpCode {
                tag: OpTag::Gap,
                i1: ai,
                i2: mi,
                j1: bj,
                j2: mj,
            });
        }
        // Extend the equal run while consecutive matches are adjacent in both.
        let run_start_i = mi;
        let run_start_j = mj;
        let mut end = k;
        while end + 1 < matches.len()
            && matches[end + 1].0 == matches[end].0 + 1
            && matches[end + 1].1 == matches[end].1 + 1
        {
            end += 1;
        }
        let end_i = matches[end].0 + 1;
        let end_j = matches[end].1 + 1;
        ops.push(OpCode {
            tag: OpTag::Equal,
            i1: run_start_i,
            i2: end_i,
            j1: run_start_j,
            j2: end_j,
        });
        ai = end_i;
        bj = end_j;
        k = end + 1;
    }
    if ai < n || bj < m {
        ops.push(OpCode {
            tag: OpTag::Gap,
            i1: ai,
            i2: n,
            j1: bj,
            j2: m,
        });
    }
    ops
}

// ---------------------------------------------------------------------------
// Template derivation
// ---------------------------------------------------------------------------

/// Returns a conservative wildcard template (space-joined, placeholders
/// collapsed) from two normalized token lists, using `a_raw`'s tokens for the
/// equal-run static text.
pub fn derive_pair_template(a_raw: &str, a_norm: &[String], b_norm: &[String]) -> String {
    let a_tokens = tokenize(a_raw);
    let ops = get_opcodes(a_norm, b_norm);
    let mut out: Vec<String> = Vec::new();
    for op in ops {
        match op.tag {
            OpTag::Equal => {
                for idx in op.i1..op.i2 {
                    let raw = if idx < a_tokens.len() {
                        a_tokens[idx].as_str()
                    } else {
                        a_norm[idx].as_str()
                    };
                    out.push(template_token(raw, &a_norm[idx]));
                }
            }
            OpTag::Gap => out.push(PLACEHOLDER.to_string()),
        }
    }
    collapse_placeholders(&out).join(" ")
}

/// Multi-member derivation: a position is static iff *all* members share the
/// same normalized token at that position (requires equal length). If members
/// are ragged (differing lengths) it falls back to the pair derivation of
/// member[0] vs member[1] (or member[0] alone for size 1).
pub fn derive_multi_template(a_raw: &str, members_norm: &[Vec<String>]) -> String {
    if members_norm.is_empty() {
        return String::new();
    }
    if members_norm.len() == 1 {
        // Single member: derive against itself -> all-static.
        return derive_pair_template(a_raw, &members_norm[0], &members_norm[0]);
    }
    let len0 = members_norm[0].len();
    let ragged = members_norm.iter().any(|m| m.len() != len0);
    if ragged {
        // Fall back to pair derivation (member[0] vs member[1]).
        return derive_pair_template(a_raw, &members_norm[0], &members_norm[1]);
    }
    let a_tokens = tokenize(a_raw);
    let mut out: Vec<String> = Vec::with_capacity(len0);
    for pos in 0..len0 {
        let first = &members_norm[0][pos];
        let all_same = members_norm.iter().all(|m| &m[pos] == first);
        if all_same {
            let raw = if pos < a_tokens.len() {
                a_tokens[pos].as_str()
            } else {
                first.as_str()
            };
            out.push(template_token(raw, first));
        } else {
            out.push(PLACEHOLDER.to_string());
        }
    }
    collapse_placeholders(&out).join(" ")
}

// ---------------------------------------------------------------------------
// Regex compilation (memory.py)
// ---------------------------------------------------------------------------

fn strip_commas(s: &str) -> String {
    s.replace(',', "")
}

/// Port of `regex_from_placeholder`, returning a *compiled* anchored regex.
/// `<*>` -> `(.*?)`, the rest escaped, commas stripped, anchored `^...$`.
/// Returns `None` if the pattern fails to compile OR has no static anchor
/// (i.e. the template is all placeholders / not useful - `is_useful_template`
/// false), matching the contract in the task spec.
pub fn regex_from_placeholder(template_text: &str) -> Option<Regex> {
    let pattern = regex_string_from_placeholder(template_text)?;
    Regex::new(&pattern).ok()
}

/// True if the template has at least one *static anchor* - a non-placeholder,
/// non-punctuation, non-whitespace char. A template that is all placeholders /
/// punctuation (`<*>`, `<*> <*>`, `: <*>`) has no anchor and would match nearly
/// every line, so its regex is refused.
fn has_static_anchor(template_text: &str) -> bool {
    let static_only = template_text.replace(PLACEHOLDER, "");
    static_only
        .chars()
        .any(|c| !ALLOWED_NOISE_PUNCT.contains(c) && c != ' ' && c != '\t')
}

/// Build the anchored regex *string* for a placeholder template, applying the
/// LibreLog comma-strip. Returns `None` if it has no static anchor or fails to
/// compile.
pub fn regex_string_from_placeholder(template_text: &str) -> Option<String> {
    if !has_static_anchor(template_text) {
        return None;
    }
    let text = strip_commas(template_text);
    let parts: Vec<&str> = text.split(PLACEHOLDER).collect();
    let escaped: Vec<String> = parts.iter().map(|p| regex::escape(p)).collect();
    let pattern = format!("^{}$", escaped.join(REGEX_VAR));
    // Validate it compiles.
    Regex::new(&pattern).ok().map(|_| pattern)
}

/// Match a line against a compiled template regex using LibreLog conventions:
/// strip commas + trim whitespace on the line before matching.
pub fn template_matches_line(re: &Regex, line: &str) -> bool {
    let prepared = strip_commas(line);
    re.is_match(prepared.trim())
}

/// Port of `is_useful_template`, parameterized by `min_static_chars`.
/// Counts static (non-placeholder, non-punctuation, non-space) chars.
pub fn is_useful_template(template_text: &str, min_static_chars: usize) -> bool {
    if template_text.trim().is_empty() {
        return false;
    }
    let static_only = template_text.replace(PLACEHOLDER, "");
    let real_chars = static_only
        .chars()
        .filter(|c| !ALLOWED_NOISE_PUNCT.contains(*c) && *c != ' ' && *c != '\t')
        .count();
    real_chars >= min_static_chars
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lcs_opcodes_equal_and_gap() {
        let a = v(&["acquired", "connection", "in_use", "5"]);
        let b = v(&["acquired", "connection", "in_use", "5"]);
        let ops = get_opcodes(&a, &b);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].tag, OpTag::Equal);
        assert_eq!((ops[0].i1, ops[0].i2), (0, 4));

        let c = v(&["x", "alpha", "z"]);
        let d = v(&["x", "beta", "z"]);
        let ops2 = get_opcodes(&c, &d);
        // equal x, gap, equal z
        assert_eq!(ops2[0].tag, OpTag::Equal);
        assert_eq!(ops2[1].tag, OpTag::Gap);
        assert_eq!(ops2[2].tag, OpTag::Equal);
    }

    #[test]
    fn adjacent_placeholder_collapse() {
        let toks = v(&["a", "<*>", "<*>", "b"]);
        assert_eq!(collapse_placeholders(&toks), v(&["a", "<*>", "b"]));
    }

    #[test]
    fn derive_pair_static_vs_vary() {
        // members differ at one token -> that becomes <*>.
        let a_raw = "acquired connection in_use 5";
        let a_norm = v(&["acquired", "connection", "in_use", "5"]);
        let b_norm = v(&["acquired", "connection", "in_use", "4218"]);
        let t = derive_pair_template(a_raw, &a_norm, &b_norm);
        assert_eq!(t, "acquired connection in_use <*>");
    }

    #[test]
    fn anchored_regex_matches_members() {
        let t = "acquired connection in_use <*>";
        let re = regex_from_placeholder(t).unwrap();
        assert!(template_matches_line(&re, "acquired connection in_use 5"));
        assert!(template_matches_line(
            &re,
            "acquired connection in_use 4218"
        ));
        assert!(!template_matches_line(&re, "released connection in_use 5"));
    }

    #[test]
    fn regex_from_placeholder_rejects_no_anchor() {
        assert!(regex_from_placeholder("<*>").is_none());
        assert!(regex_from_placeholder("<*> <*>").is_none());
    }

    #[test]
    fn is_useful_template_threshold() {
        assert!(!is_useful_template("<*>", 3));
        assert!(!is_useful_template(": <*>", 3));
        assert!(is_useful_template("acquired <*>", 3));
        // exactly 3 static chars passes
        assert!(is_useful_template("abc <*>", 3));
        assert!(!is_useful_template("ab <*>", 3));
    }

    #[test]
    fn derive_multi_static_vs_vary() {
        let a_raw = "pool size 10 ready true";
        let m0 = v(&["pool", "size", "10", "ready", "true"]);
        let m1 = v(&["pool", "size", "12", "ready", "true"]);
        let m2 = v(&["pool", "size", "99", "ready", "false"]);
        // positions 2 and 4 vary -> placeholders.
        let t = derive_multi_template(a_raw, &[m0, m1, m2]);
        assert_eq!(t, "pool size <*> ready <*>");
    }

    #[test]
    fn derive_multi_ragged_falls_back_to_pair() {
        let a_raw = "a b c";
        let m0 = v(&["a", "b", "c"]);
        let m1 = v(&["a", "b"]);
        let m2 = v(&["a", "b", "c", "d"]);
        // ragged -> pair derivation of m0 vs m1: equal "a b", gap -> <*>
        let t = derive_multi_template(a_raw, &[m0, m1, m2]);
        assert_eq!(t, "a b <*>");
    }

    #[test]
    fn single_member_all_static() {
        let a_raw = "started worker 7";
        let m0 = v(&["started", "worker", "7"]);
        let t = derive_multi_template(a_raw, &[m0]);
        assert_eq!(t, "started worker 7");
    }
}
