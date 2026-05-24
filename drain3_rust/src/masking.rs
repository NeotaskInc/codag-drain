use regex::Regex;

/// A single masking rule: a compiled regex and the token to replace matches with.
#[derive(Clone)]
pub struct MaskingInstruction {
    pub regex: Regex,
    pub mask_with: String,
}

impl MaskingInstruction {
    pub fn new(pattern: &str, mask_with: &str) -> Self {
        Self {
            regex: Regex::new(pattern).expect("invalid masking regex"),
            mask_with: mask_with.to_string(),
        }
    }
}

/// Applies a sequence of regex substitutions to log content before parsing.
///
/// Each [`MaskingInstruction`] is applied in order, replacing all matches with
/// `{mask_prefix}{mask_with}{mask_suffix}` (e.g. `<NUM>`).
#[derive(Clone)]
pub struct LogMasker {
    instructions: Vec<MaskingInstruction>,
    pub mask_prefix: String,
    pub mask_suffix: String,
}

impl LogMasker {
    pub fn new(instructions: Vec<MaskingInstruction>) -> Self {
        Self {
            instructions,
            mask_prefix: "<".to_string(),
            mask_suffix: ">".to_string(),
        }
    }

    /// Apply all masking rules sequentially, returning the masked string.
    pub fn mask(&self, content: &str) -> String {
        let mut result = content.to_string();
        for inst in &self.instructions {
            let replacement = format!("{}{}{}", self.mask_prefix, inst.mask_with, self.mask_suffix);
            result = inst
                .regex
                .replace_all(&result, replacement.as_str())
                .into_owned();
        }
        result
    }
}

/// Returns common masking patterns matching Python Drain3 defaults:
/// - IP addresses
/// - Hex sequences (0x...)
/// - Numbers at word boundaries
pub fn default_masking_instructions() -> Vec<MaskingInstruction> {
    vec![
        MaskingInstruction::new(r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}", "IP"),
        MaskingInstruction::new(r"0x[0-9a-fA-F]+", "HEX"),
        MaskingInstruction::new(r"\b[\-\+]?\d+\b", "NUM"),
    ]
}
