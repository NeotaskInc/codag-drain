//! Structural / typed normalization for grouping.
//!
//! Faithful port of `v2/src/v2/templater/grouping.py`:
//!   - `tokenize`
//!   - `mask_path_segments`
//!   - `normalize_for_grouping`        (typed skeleton)
//!   - `structural_normalize_for_grouping` (aggressive structural skeleton)
//!   - `jaccard`
//!
//! Rust's `regex` crate has no lookaround/backreferences, so five Python
//! patterns are rewritten imperatively. Each rewrite site is annotated with a
//! `// REWRITE:` comment describing the original Python pattern.

use regex::Regex;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Lazily-compiled regexes (mirrors the module-level Python `re.compile`s).
// ---------------------------------------------------------------------------

fn numeric_re() -> &'static Regex {
    // _NUMERIC_RE = r"^-?\d+(?:\.\d+)?$"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^-?\d+(?:\.\d+)?$").unwrap())
}

fn http_status_re() -> &'static Regex {
    // _HTTP_STATUS_RE = r"^[1-5]\d\d$"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[1-5]\d\d$").unwrap())
}

fn duration_re() -> &'static Regex {
    // _DURATION_RE = r"\b\d+(?:\.\d+)?(?:ns|us|µs|ms|s|m|h)\b", IGNORECASE
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b\d+(?:\.\d+)?(?:ns|us|µs|ms|s|m|h)\b").unwrap())
}

fn size_re() -> &'static Regex {
    // _SIZE_RE = r"\b\d+(?:\.\d+)?(?:b|kb|kib|mb|mib|gb|gib|tb|tib)\b", IGNORECASE
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b\d+(?:\.\d+)?(?:b|kb|kib|mb|mib|gb|gib|tb|tib)\b").unwrap())
}

fn kv_status_re() -> &'static Regex {
    // _KV_STATUS_RE = r"\b(status|code)[=:]([1-5]\d\d)\b", IGNORECASE
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(status|code)[=:]([1-5]\d\d)\b").unwrap())
}

fn kv_number_re() -> &'static Regex {
    // _KV_NUMBER_RE = r"(?<=[=:])[-+]?\d+(?:\.\d+)?"  (lookbehind)
    // REWRITE #5: Rust regex has no lookbehind. Capture the separator instead of
    // looking behind it: `([=:])([-+]?\d+(?:\.\d+)?)` and substitute `${1}<NUM>`
    // (consume the separator and re-emit it), which yields identical output.
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"([=:])([-+]?\d+(?:\.\d+)?)").unwrap())
}

fn uuid_any_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
            .unwrap()
    })
}

fn ip_port_re() -> &'static Regex {
    // _IP_PORT_RE = r"/?\b\d{1,3}(?:\.\d{1,3}){3}(?::\d+)?"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"/?\b\d{1,3}(?:\.\d{1,3}){3}(?::\d+)?").unwrap())
}

fn hex_any_re() -> &'static Regex {
    // _HEX_ANY_RE = r"\b(?:0x)?[0-9a-fA-F]{8,}\b"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b(?:0x)?[0-9a-fA-F]{8,}\b").unwrap())
}

fn func_offset_re() -> &'static Regex {
    // _FUNC_OFFSET_RE = r"^([A-Za-z_][A-Za-z0-9_.$-]*?)\+(?:0x)?[0-9a-fA-F]+(?:/(?:0x)?[0-9a-fA-F]+)?([,.)\]]?)$"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"^([A-Za-z_][A-Za-z0-9_.$-]*?)\+(?:0x)?[0-9a-fA-F]+(?:/(?:0x)?[0-9a-fA-F]+)?([,.)\]]?)$",
        )
        .unwrap()
    })
}

fn blockish_id_re() -> &'static Regex {
    // _BLOCKISH_ID_RE, IGNORECASE
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:blk|job|task|attempt|app|container|application|BP|DS)[_-][A-Za-z0-9_.:-]+\b",
        )
        .unwrap()
    })
}

fn hostport_re() -> &'static Regex {
    // _HOSTPORT_RE = r"^[A-Za-z0-9_.-]+:\d+[,.)\]]?$", IGNORECASE
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^[A-Za-z0-9_.-]+:\d+[,.)\]]?$").unwrap())
}

fn domain_body_re() -> &'static Regex {
    // _DOMAIN_RE = r"^(?=.{4,253}$)(?:[A-Za-z0-9-]+\.)+[A-Za-z]{2,}(?::\d+)?[,.)\]]?$"
    // REWRITE #3: the `(?=.{4,253}$)` length lookahead is dropped from the body
    // regex and enforced imperatively via `(4..=253).contains(&len)` in
    // `domain_match`. The remaining body is anchored as-is.
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(?:[A-Za-z0-9-]+\.)+[A-Za-z]{2,}(?::\d+)?[,.)\]]?$").unwrap())
}

fn urlpath_re() -> &'static Regex {
    // _URLPATH_RE = r"[\"']?/?(?:[A-Za-z0-9_.-]+/)+[A-Za-z0-9_./:@#%&?=+-]+[,.)\]\"']?$"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"["']?/?(?:[A-Za-z0-9_.-]+/)+[A-Za-z0-9_./:@#%&?=+-]+[,.)\]"']?$"#).unwrap()
    })
}

fn hash_num_re() -> &'static Regex {
    // _HASH_NUM_RE = r"^\d+(?:##\d+)+$"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\d+(?:##\d+)+$").unwrap())
}

fn numish_re() -> &'static Regex {
    // _NUMISH_RE = r"^[\[({<]*[-+]?\d+(?:\.\d+)?(?:[A-Za-z%]+)?[,.)\]>]*$"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[\[({<]*[-+]?\d+(?:\.\d+)?(?:[A-Za-z%]+)?[,.)\]>]*$").unwrap())
}

fn unitish_re() -> &'static Regex {
    // _UNITISH_RE, IGNORECASE
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)^[\[({<]*(?:B|KB|KiB|MB|MiB|GB|GiB|TB|ms|sec|secs|s|m|h)[,.)\]>]*$")
            .unwrap()
    })
}

fn mixed_id_body_re() -> &'static Regex {
    // _MIXED_ID_RE = r"^(?=.*\d)(?=.*[A-Za-z])[A-Za-z0-9_./:@#%&+=-]{3,}[,.)\]]?$", IGNORECASE
    // REWRITE #4: the two `(?=.*\d)` / `(?=.*[A-Za-z])` lookaheads (must contain
    // at least one digit AND at least one ascii letter) are checked imperatively
    // in `mixed_id_match`; the body alphabet/length is matched by this regex.
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^[A-Za-z0-9_./:@#%&+=-]{3,}[,.)\]]?$").unwrap())
}

fn kv_re() -> &'static Regex {
    // _KV_RE = r"^([A-Za-z_][\w.-]*)([:=])(.+)$"
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^([A-Za-z_][\w.-]*)([:=])(.+)$").unwrap())
}

// _PATH_VARS, applied inside each token. The numeric-path-segment entry
// (`/\d+(?=/|$|\?)`) is rewritten imperatively (see `mask_numeric_path_segments`).
fn iso_ts_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?")
            .unwrap()
    })
}
fn ipv4_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(?::\d+)?").unwrap())
}
fn uuid_word_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
        )
        .unwrap()
    })
}
fn hex8_word_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b[0-9a-fA-F]{8,}\b").unwrap())
}
fn query_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"\?[^\s"']+"#).unwrap())
}
fn bracket_num_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[\[\(]\d+[\]\)]").unwrap())
}

// ---------------------------------------------------------------------------
// Static word sets (mirror the Python module-level sets).
// ---------------------------------------------------------------------------

const LEVELS: &[&str] = &[
    "trace", "debug", "info", "notice", "warn", "warning", "error", "err", "fatal", "critical",
    "crit",
];
const BOOLS: &[&str] = &["true", "false", "yes", "no"];

fn is_level(s: &str) -> bool {
    LEVELS.contains(&s)
}
fn is_bool(s: &str) -> bool {
    BOOLS.contains(&s)
}

/// Python `str.strip("[](){}<>.,;:'\"")` — trim those chars from both ends.
fn strip_punct(s: &str) -> &str {
    s.trim_matches(|c| "[](){}<>.,;:'\"".contains(c))
}

// ---------------------------------------------------------------------------
// Imperative rewrites of lookaround/backref patterns.
// ---------------------------------------------------------------------------

/// REWRITE #1: Python `_QUOTED_RE = r"^(['\"]).*\1$"` uses a backreference `\1`
/// to require the same quote char at both ends. Rust regex has no backrefs, so
/// we check it directly: first char == last char, that char is `"` or `'`, and
/// the token has length >= 2.
fn is_quoted(tok: &str) -> bool {
    let chars: Vec<char> = tok.chars().collect();
    if chars.len() < 2 {
        return false;
    }
    let first = chars[0];
    let last = chars[chars.len() - 1];
    (first == '"' || first == '\'') && first == last
}

/// REWRITE #2: Python `(re.compile(r"/\d+(?=/|$|\?)"), "/*")` uses a lookahead so
/// that a pure-numeric path segment is replaced only when followed by `/`, `?`,
/// or end-of-string. Rust regex has no lookahead, so we split the token on `/`,
/// replace any all-digit segment with the sentinel `*`, and rejoin with `/`.
/// The trailing `/`, `?`, or end is honored structurally by the split/rejoin
/// (a query string portion is masked separately by `query_re`). Each emitted
/// segment carries `/` + segment, matching the Python `/\d+` -> `/*` mapping.
fn mask_numeric_path_segments(tok: &str) -> String {
    if !tok.contains('/') {
        return tok.to_string();
    }
    // Split off a trailing query string so a `?...` boundary is preserved; the
    // query portion is handled by the separate `?[^\s"']+` -> `?*` rule later.
    let (path_part, query_part) = match tok.find('?') {
        Some(qpos) => (&tok[..qpos], &tok[qpos..]),
        None => (tok, ""),
    };
    let segments: Vec<&str> = path_part.split('/').collect();
    let masked: Vec<String> = segments
        .iter()
        .map(|seg| {
            if !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()) {
                "*".to_string()
            } else {
                (*seg).to_string()
            }
        })
        .collect();
    format!("{}{}", masked.join("/"), query_part)
}

/// REWRITE #3 helper: domain detection with the `(?=.{4,253}$)` length lookahead
/// enforced imperatively.
fn domain_match(tok: &str) -> bool {
    (4..=253).contains(&tok.len()) && domain_body_re().is_match(tok)
}

/// REWRITE #4 helper: mixed-id detection with the two `(?=.*\d)`/`(?=.*[A-Za-z])`
/// lookaheads enforced imperatively (must contain a digit AND a letter).
fn mixed_id_match(tok: &str) -> bool {
    tok.chars().any(|c| c.is_ascii_digit())
        && tok.chars().any(|c| c.is_ascii_alphabetic())
        && mixed_id_body_re().is_match(tok)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Whitespace tokenize (Python `line.split()`).
pub fn tokenize(line: &str) -> Vec<String> {
    line.split_whitespace().map(|s| s.to_string()).collect()
}

/// Token-set Jaccard similarity. Empty/empty -> 0.0 (matches Python `jaccard`).
pub fn jaccard(a: &[String], b: &[String]) -> f64 {
    use std::collections::HashSet;
    let set_a: HashSet<&String> = a.iter().collect();
    let set_b: HashSet<&String> = b.iter().collect();
    if set_a.is_empty() && set_b.is_empty() {
        return 0.0;
    }
    let inter = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// `mask_path_segments`: collapse high-cardinality structures inside each token.
/// Order of the substitutions mirrors `_PATH_VARS` exactly.
fn mask_path_token(tok: &str) -> String {
    let mut masked = tok.to_string();
    masked = iso_ts_re().replace_all(&masked, "*").into_owned();
    masked = ipv4_re().replace_all(&masked, "*").into_owned();
    masked = uuid_word_re().replace_all(&masked, "*").into_owned();
    masked = hex8_word_re().replace_all(&masked, "*").into_owned();
    // REWRITE #2 site: numeric path segment (`/\d+(?=/|$|\?)` -> `/*`).
    masked = mask_numeric_path_segments(&masked);
    masked = query_re().replace_all(&masked, "?*").into_owned();
    masked = bracket_num_re().replace_all(&masked, "*").into_owned();
    masked
}

fn mask_path_segments(tokens: &[String]) -> Vec<String> {
    tokens.iter().map(|t| mask_path_token(t)).collect()
}

/// The `Normalizer` holds no state but provides the typed/structural skeletons.
#[derive(Debug, Default, Clone)]
pub struct Normalizer;

impl Normalizer {
    pub fn new() -> Self {
        Normalizer
    }

    /// Port of `normalize_for_grouping`: typed sentinels, static words intact.
    pub fn normalize_for_grouping(&self, line: &str) -> Vec<String> {
        let tokens = tokenize(line);
        if tokens.is_empty() {
            return vec![String::new()];
        }
        let path_masked = mask_path_segments(&tokens);
        let mut out: Vec<String> = Vec::with_capacity(path_masked.len());
        for raw in &path_masked {
            let tok = raw.replace('*', "<VAR>");
            let bare = strip_punct(&tok).to_lowercase();
            if is_level(&bare) {
                out.push("<LEVEL>".to_string());
                continue;
            }
            if http_status_re().is_match(&bare) {
                let first = bare.chars().next().unwrap();
                out.push(format!("<HTTP_{first}XX>"));
                continue;
            }
            if is_quoted(&tok) {
                // REWRITE #1 site.
                out.push("<STR>".to_string());
                continue;
            }
            if numeric_re().is_match(&tok) {
                out.push("<NUM>".to_string());
                continue;
            }
            let mut t = tok.clone();
            t = duration_re().replace_all(&t, "<VAL>").into_owned();
            t = size_re().replace_all(&t, "<VAL>").into_owned();
            t = kv_status_re()
                .replace_all(&t, |c: &regex::Captures| {
                    let key = &c[1];
                    let status = &c[2];
                    let first = status.chars().next().unwrap();
                    format!("{key}=<HTTP_{first}XX>")
                })
                .into_owned();
            // REWRITE #5 site: lookbehind `(?<=[=:])` -> capture the separator.
            t = kv_number_re().replace_all(&t, "${1}<NUM>").into_owned();
            out.push(t);
        }
        out
    }

    /// Port of `structural_normalize_for_grouping`.
    pub fn structural_normalize_for_grouping(&self, line: &str) -> Vec<String> {
        let tokens = tokenize(line);
        if tokens.is_empty() {
            return vec![String::new()];
        }
        let mut out: Vec<String> = Vec::with_capacity(tokens.len());
        for (idx, raw) in tokens.iter().enumerate() {
            let tok = raw.trim();
            let bare = strip_punct(tok);
            let low = bare.to_lowercase();

            // _FUNC_OFFSET_RE.match(tok.strip("[]<>"))
            let func_input = tok.trim_matches(|c| "[]<>".contains(c));
            if let Some(c) = func_offset_re().captures(func_input) {
                out.push(format!("{}+<VAL>{}", &c[1], &c[2]));
                continue;
            }
            if uuid_any_re().is_match(tok) {
                out.push("<ID>".to_string());
                continue;
            }
            if ip_port_re().is_match(tok) {
                out.push("<ADDR>".to_string());
                continue;
            }
            if hostport_re().is_match(tok) || domain_match(tok) {
                out.push("<ADDR>".to_string());
                continue;
            }
            if blockish_id_re().is_match(tok) {
                out.push("<ID>".to_string());
                continue;
            }
            if urlpath_re().is_match(tok) {
                out.push("<PATH>".to_string());
                continue;
            }
            if hash_num_re().is_match(bare) {
                out.push("<ID>".to_string());
                continue;
            }
            if hex_any_re().is_match(tok) {
                out.push("<ID>".to_string());
                continue;
            }
            if numish_re().is_match(tok) {
                out.push("<NUM>".to_string());
                continue;
            }
            if unitish_re().is_match(tok) {
                out.push("<UNIT>".to_string());
                continue;
            }
            if mixed_id_match(tok) {
                out.push("<ID>".to_string());
                continue;
            }

            // kv = _KV_RE.match(tok.strip("[](){}<>.,;:'\""))
            let kv_input = strip_punct(tok);
            if let Some(c) = kv_re().captures(kv_input) {
                if looks_like_structural_value(&c[3]) {
                    out.push(format!("{}{}<VAL>", &c[1], &c[2]));
                    continue;
                }
            }

            if idx <= 3 && is_level(&low) {
                out.push("<LEVEL>".to_string());
                continue;
            }
            if is_bool(&low) {
                out.push("<BOOL>".to_string());
                continue;
            }
            out.push(tok.to_string());
        }
        out
    }
}

/// Port of `_looks_like_structural_value`.
fn looks_like_structural_value(value: &str) -> bool {
    let stripped = strip_punct(value);
    let lowered = stripped.to_lowercase();
    stripped.is_empty()
        || is_bool(&lowered)
        || uuid_any_re().is_match(stripped)
        || ip_port_re().is_match(stripped)
        || hex_any_re().is_match(stripped)
        || blockish_id_re().is_match(stripped)
        || hostport_re().is_match(stripped)
        || domain_match(stripped)
        || urlpath_re().is_match(stripped)
        || hash_num_re().is_match(stripped)
        || numish_re().is_match(stripped)
        || unitish_re().is_match(stripped)
        || mixed_id_match(stripped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n() -> Normalizer {
        Normalizer::new()
    }

    #[test]
    fn tokenize_splits_whitespace() {
        assert_eq!(tokenize("a  b\tc"), vec!["a", "b", "c"]);
        assert_eq!(tokenize(""), Vec::<String>::new());
    }

    #[test]
    fn jaccard_basic() {
        assert_eq!(jaccard(&[], &[]), 0.0);
        let a = vec!["x".to_string(), "y".to_string()];
        let b = vec!["y".to_string(), "z".to_string()];
        assert!((jaccard(&a, &b) - (1.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn numeric_mask() {
        assert_eq!(n().normalize_for_grouping("count 42"), vec!["count", "<NUM>"]);
        assert_eq!(n().normalize_for_grouping("v -3.5"), vec!["v", "<NUM>"]);
    }

    #[test]
    fn level_mask() {
        assert_eq!(n().normalize_for_grouping("ERROR boom")[0], "<LEVEL>");
    }

    #[test]
    fn http_status_mask() {
        assert_eq!(n().normalize_for_grouping("status 503")[1], "<HTTP_5XX>");
        assert_eq!(n().normalize_for_grouping("got 200 ok")[1], "<HTTP_2XX>");
    }

    #[test]
    fn rewrite1_quoted_detect() {
        // is_quoted boundaries.
        assert!(is_quoted("\"hello\""));
        assert!(is_quoted("'x'"));
        assert!(!is_quoted("\""));
        assert!(!is_quoted("\"abc")); // mismatched ends
        assert!(!is_quoted("'abc\"")); // different quote chars
        // A quoted string is only `<STR>` when it is a single whitespace token
        // (the Python tokenizer splits on whitespace, so `"hi there"` is two
        // tokens and never matches the anchored quote regex).
        assert_eq!(n().normalize_for_grouping("said \"hello\"")[1], "<STR>");
    }

    #[test]
    fn rewrite2_numeric_path_segment() {
        // /orders/9/line-items/17 -> /orders/*/line-items/*
        let out = mask_path_token("/orders/9/line-items/17");
        assert_eq!(out, "/orders/*/line-items/*");
        // trailing query honored: /v1/users/42?x=1 -> /v1/users/*?* (query masked later)
        let out2 = mask_path_token("/v1/users/42?id=1");
        assert_eq!(out2, "/v1/users/*?*");
        // non-numeric segment untouched
        assert_eq!(mask_path_token("/orders/abc"), "/orders/abc");
        // numeric segment at end
        assert_eq!(mask_path_token("/x/7"), "/x/*");
    }

    #[test]
    fn rewrite3_domain_length_gate() {
        // len 3 reject ("a.b" is len 3 -> reject)
        assert!(!domain_match("a.b"));
        // len 4 accept ("a.io" is len 4)
        assert!(domain_match("a.io"));
        // len 254 reject
        let long = format!("{}.com", "a".repeat(250)); // 250 + 4 = 254
        assert_eq!(long.len(), 254);
        assert!(!domain_match(&long));
    }

    #[test]
    fn rewrite4_mixed_id() {
        // digit-only reject
        assert!(!mixed_id_match("12345"));
        // alpha-only reject
        assert!(!mixed_id_match("abcdef"));
        // mixed accept
        assert!(mixed_id_match("abc123"));
        assert!(mixed_id_match("req-9f2"));
    }

    #[test]
    fn rewrite5_kv_number() {
        assert_eq!(n().normalize_for_grouping("x=5")[0], "x=<NUM>");
        assert_eq!(n().normalize_for_grouping("retries:3")[0], "retries:<NUM>");
        // negative / float
        assert_eq!(n().normalize_for_grouping("delta=-2.5")[0], "delta=<NUM>");
    }

    #[test]
    fn duration_and_size_mask() {
        assert_eq!(n().normalize_for_grouping("took 45ms")[1], "<VAL>");
        assert_eq!(n().normalize_for_grouping("used 512MB")[1], "<VAL>");
    }

    #[test]
    fn structural_uuid_and_ip() {
        let out = n().structural_normalize_for_grouping(
            "conn 550e8400-e29b-41d4-a716-446655440000 from 10.0.0.1:5432",
        );
        assert!(out.contains(&"<ID>".to_string()));
        assert!(out.contains(&"<ADDR>".to_string()));
    }

    #[test]
    fn structural_keeps_static_words() {
        let out = n().structural_normalize_for_grouping("acquired connection from pool");
        assert_eq!(out, vec!["acquired", "connection", "from", "pool"]);
    }

    #[test]
    fn structural_func_offset_frame() {
        let out = n().structural_normalize_for_grouping("do_page_fault+0x1f2/0x550");
        assert_eq!(out, vec!["do_page_fault+<VAL>"]);
    }
}
