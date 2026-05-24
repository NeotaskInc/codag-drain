//! # codag-drain
//!
//! Deterministic log templating adapted from Drain3 for `codag wrap -- ...`.
//!
//! This crate groups log lines, derives templates, and returns template groups
//! with bounded samples and slot summaries. It does not build incidents or make
//! diagnostic claims; those belong to the inference pipeline.

pub mod compress;
pub mod input;
pub mod stream;

pub use compress::{
    template_groups, template_logs, GrouperKind, LogLine, SlotSummary, TemplateGroup,
    TemplateResult, TemplateSample, TemplaterConfig,
};
pub use input::{parse_body, parse_json_line, parse_line, BodyFormat};

pub use stream::TemplateIndex;
