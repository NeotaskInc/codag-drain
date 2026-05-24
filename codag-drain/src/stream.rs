//! In-memory template index for a log window.
//!
//! The index is intentionally lightweight state for the thin server wrapper:
//! it accumulates lines and projects deterministic template groups on demand.
//! It does not model incidents.

use crate::compress::{template_logs, LogLine, TemplateResult, TemplaterConfig};

/// In-memory log window.
///
/// `push()` accumulates raw parsed lines. `templates_with()` runs the selected
/// batch templater over the current window so the grouping algorithm can use
/// the whole context.
#[derive(Debug, Clone)]
pub struct TemplateIndex {
    config: TemplaterConfig,
    lines: Vec<LogLine>,
}

impl TemplateIndex {
    pub fn new(config: TemplaterConfig) -> Self {
        TemplateIndex {
            config,
            lines: Vec::new(),
        }
    }

    /// Ingest one line.
    pub fn push(&mut self, line: LogLine) {
        self.lines.push(line);
    }

    pub fn extend<I: IntoIterator<Item = LogLine>>(&mut self, lines: I) {
        for line in lines {
            self.push(line);
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn templates(&self) -> TemplateResult {
        self.templates_with(&self.config)
    }

    pub fn templates_with(&self, config: &TemplaterConfig) -> TemplateResult {
        template_logs(&self.lines, config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(msg: &str) -> LogLine {
        LogLine::new(msg.to_string())
    }

    fn pushed(lines: &[LogLine], cfg: &TemplaterConfig) -> TemplateIndex {
        let mut idx = TemplateIndex::new(cfg.clone());
        for l in lines {
            idx.push(l.clone());
        }
        idx
    }

    #[test]
    fn len_and_is_empty() {
        let mut idx = TemplateIndex::new(TemplaterConfig::default());
        assert!(idx.is_empty());
        assert_eq!(idx.len(), 0);
        idx.push(line("hello world"));
        idx.push(line("hello world"));
        assert!(!idx.is_empty());
        assert_eq!(idx.len(), 2);
    }

    #[test]
    fn empty_templates() {
        let idx = TemplateIndex::new(TemplaterConfig::default());
        let r = idx.templates();
        assert_eq!(r.original_count, 0);
        assert_eq!(r.template_count, 0);
        assert!(r.groups.is_empty());
        assert_eq!(r.render(), "");
    }

    #[test]
    fn per_request_config_matches_batch() {
        let lines = vec![
            line("node phase changed to Succeeded now"),
            line("node phase changed to Failed now"),
            line("node phase changed to Skipped now"),
        ];
        let idx = pushed(&lines, &TemplaterConfig::default());
        let drain = TemplaterConfig::default();
        assert_eq!(idx.templates_with(&drain), template_logs(&lines, &drain));
    }

    #[test]
    fn repeated_projection_is_stable() {
        let lines = vec![
            line("GET /users/1 200"),
            line("GET /users/2 200"),
            line("GET /users/3 200"),
        ];
        let idx = pushed(&lines, &TemplaterConfig::default());
        assert_eq!(idx.templates(), idx.templates());
    }
}
