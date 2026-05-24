//! Generic lexical tokenization for data-driven grouping/template derivation.
//!
//! This module intentionally avoids log-domain shape rules. It only separates
//! text into generic lexical classes (word/number-ish runs, punctuation, other)
//! and preserves byte spans back to the original line.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexKind {
    Word,
    Punct,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexToken {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub kind: LexKind,
}

fn char_kind(c: char) -> Option<LexKind> {
    if c.is_whitespace() {
        None
    } else if c.is_alphanumeric() || c == '_' || c == '-' {
        Some(LexKind::Word)
    } else if c.is_ascii_punctuation() {
        Some(LexKind::Punct)
    } else {
        Some(LexKind::Other)
    }
}

/// Tokenize by generic character classes while preserving byte spans.
pub fn lex(line: &str) -> Vec<LexToken> {
    let mut out = Vec::new();
    let mut cur_start: Option<usize> = None;
    let mut cur_kind: Option<LexKind> = None;

    for (idx, ch) in line.char_indices() {
        let kind = char_kind(ch);
        match (cur_start, cur_kind, kind) {
            (None, _, Some(k)) => {
                cur_start = Some(idx);
                cur_kind = Some(k);
            }
            (Some(start), Some(k0), Some(k1)) if k0 != k1 || k1 == LexKind::Punct => {
                out.push(LexToken {
                    text: line[start..idx].to_string(),
                    start,
                    end: idx,
                    kind: k0,
                });
                cur_start = Some(idx);
                cur_kind = Some(k1);
            }
            (Some(start), Some(k0), None) => {
                out.push(LexToken {
                    text: line[start..idx].to_string(),
                    start,
                    end: idx,
                    kind: k0,
                });
                cur_start = None;
                cur_kind = None;
            }
            _ => {}
        }
    }

    if let (Some(start), Some(kind)) = (cur_start, cur_kind) {
        out.push(LexToken {
            text: line[start..].to_string(),
            start,
            end: line.len(),
            kind,
        });
    }

    out
}

fn is_anchor(tok: &LexToken) -> bool {
    tok.kind != LexKind::Punct
        && !tok.text.chars().any(|c| c.is_ascii_digit())
        && tok.text.chars().any(|c| c.is_alphanumeric() || c == '_')
}

/// Render a multi-member template from lexical tokens.
///
/// Positions are static only when every member has the same token text at that
/// position. Dynamic positions become `<*>`. Ragged groups return `None`.
pub fn derive_lex_template(raw: &str, members: &[Vec<LexToken>]) -> Option<String> {
    let first = members.first()?;
    let len = first.len();
    if members.iter().any(|m| m.len() != len) {
        return None;
    }

    let mut out = String::with_capacity(raw.len());
    let mut cursor = 0usize;
    let mut last_placeholder = false;

    for pos in 0..len {
        let tok = &first[pos];
        out.push_str(&raw[cursor..tok.start]);
        let static_pos = members.iter().all(|m| m[pos].text == tok.text);
        if static_pos {
            out.push_str(&raw[tok.start..tok.end]);
            last_placeholder = false;
        } else if !last_placeholder {
            out.push_str(crate::compress::template::PLACEHOLDER);
            last_placeholder = true;
        }
        cursor = tok.end;
    }
    out.push_str(&raw[cursor..]);
    Some(out)
}

/// Count non-punctuation static anchors in a token vector/template mask.
pub fn anchor_count(tokens: &[LexToken], static_mask: &[bool]) -> usize {
    tokens
        .iter()
        .zip(static_mask.iter())
        .filter(|(t, &is_static)| is_static && is_anchor(t))
        .count()
}

pub fn anchor_chars(tokens: &[LexToken], static_mask: &[bool]) -> usize {
    tokens
        .iter()
        .zip(static_mask.iter())
        .filter(|(t, &is_static)| is_static && is_anchor(t))
        .map(|(t, _)| t.text.chars().filter(|c| c.is_alphanumeric()).count())
        .sum()
}

pub fn token_is_anchor(tok: &LexToken) -> bool {
    is_anchor(tok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lex_splits_compact_json() {
        let toks = lex(r#"{"level":"warning","time":"2025-01-08T08:18:33Z"}"#);
        let texts: Vec<&str> = toks.iter().map(|t| t.text.as_str()).collect();
        assert!(texts.contains(&"level"));
        assert!(texts.contains(&"warning"));
        assert!(texts.iter().any(|t| t.starts_with("2025-01-08")));
        assert!(texts.contains(&":"));
    }

    #[test]
    fn lex_splits_stack_frame_path() {
        let toks = lex(r#"File "a/b.py", line 42, in handler"#);
        let texts: Vec<&str> = toks.iter().map(|t| t.text.as_str()).collect();
        assert!(texts.contains(&"File"));
        assert!(texts.contains(&"a"));
        assert!(texts.contains(&"b"));
        assert!(texts.contains(&"py"));
        assert!(texts.contains(&"42"));
    }

    #[test]
    fn derive_template_preserves_punctuation() {
        let a = r#"{"time":"t1","msg":"ok"}"#;
        let b = r#"{"time":"t2","msg":"ok"}"#;
        let t = derive_lex_template(a, &[lex(a), lex(b)]).unwrap();
        assert_eq!(t, r#"{"time":"<*>","msg":"ok"}"#);
    }
}
