/// Compute token-level similarity between a template (`seq1`) and a log message (`seq2`).
///
/// Returns `(similarity, wildcard_count)` where similarity is the fraction of
/// non-wildcard tokens that match, and wildcard_count is the number of wildcard
/// positions in `seq1`.
///
/// When `include_params` is true, wildcard positions are counted as matches.
pub fn get_seq_distance(
    seq1: &[String],
    seq2: &[String],
    include_params: bool,
    param_str: &str,
) -> (f64, usize) {
    assert_eq!(seq1.len(), seq2.len());

    if seq1.is_empty() {
        return (1.0, 0);
    }

    let mut sim_tokens = 0usize;
    let mut param_count = 0usize;

    for (t1, t2) in seq1.iter().zip(seq2.iter()) {
        if t1 == param_str {
            param_count += 1;
            continue;
        }
        if t1 == t2 {
            sim_tokens += 1;
        }
    }

    if include_params {
        sim_tokens += param_count;
    }

    let ret_val = sim_tokens as f64 / seq1.len() as f64;
    (ret_val, param_count)
}

/// Merge two equal-length token sequences into a template.
///
/// Matching positions keep the shared token; mismatches become `param_str`.
pub fn create_template(seq1: &[String], seq2: &[String], param_str: &str) -> Vec<String> {
    assert_eq!(
        seq1.len(),
        seq2.len(),
        "create_template requires equal-length sequences"
    );
    seq1.iter()
        .zip(seq2.iter())
        .map(|(t1, t2)| {
            if t1 == t2 {
                t2.clone()
            } else {
                param_str.to_string()
            }
        })
        .collect()
}
