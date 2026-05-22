//! Per-group template + per-slot numeric profiling.
//!
//! Adapted from the `Profile` class in
//! `codag-bench/scripts/det_compressors.py`. For each group we derive a template
//! (`derive_pair_template` of members[0] vs members[1]; member[0] alone for size
//! 1), compile a `regex_from_placeholder` capture regex, capture each line's raw
//! slot strings, and compute per-slot numeric `(min, median, max)` over ALL
//! parsed values (a slot is numeric iff >= 50% of present values parse as a
//! number). The median is over the FULL list (incl. duplicates), not distinct.

use regex::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::compress::grouper::Group;
use crate::compress::normalize::Normalizer;
use crate::compress::template::{derive_pair_template, regex_from_placeholder};
use crate::compress::LogLine;

/// `NUM = re.compile(r"-?\d+(?:\.\d+)?")` — first numeric substring of a slot.
fn num_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"-?\d+(?:\.\d+)?").unwrap())
}

/// Numeric stats for one slot of one group.
#[derive(Debug, Clone)]
pub struct SlotNumeric {
    pub min: f64,
    pub median: f64,
    pub max: f64,
}

/// Per-group profile data.
#[derive(Debug, Clone)]
pub struct GroupProfile {
    /// Derived `<*>` template string.
    pub template: String,
    /// Compiled capture regex (None if it didn't compile / no static anchor).
    pub regex: Option<Regex>,
    /// Number of `<*>` slots in the template.
    pub slot_count: usize,
    /// raw_slots[member_position][slot_index] = captured raw string (or None).
    /// member_position indexes into the group's ascending member_indices.
    pub raw_slots: Vec<Vec<Option<String>>>,
    /// numeric[slot] = Some((min,median,max)) iff the slot is numeric.
    pub numeric: Vec<Option<SlotNumeric>>,
}

/// The full incident profile: one `GroupProfile` per group, plus the per-line
/// group assignment.
#[derive(Debug, Clone)]
pub struct Profile {
    /// gid[line_index] = group index (into `groups` / `profiles`).
    pub gid: Vec<usize>,
    pub profiles: Vec<GroupProfile>,
    pub outlier_factor: f64,
}

/// Median over the FULL slice (incl. duplicates). Sorts a copy. Even-length =
/// mean of the two middle elements (Python `statistics.median`).
fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

impl Profile {
    /// Build the profile. `groups` are emitted in ascending first-member-index
    /// order; `gid[i]` maps line `i` to its group index.
    pub fn build(
        lines: &[LogLine],
        groups: &[Group],
        norm: &Normalizer,
        outlier_factor: f64,
    ) -> Profile {
        let mut gid = vec![0usize; lines.len()];
        for (g_idx, g) in groups.iter().enumerate() {
            for &m in &g.member_indices {
                gid[m] = g_idx;
            }
        }

        let mut profiles: Vec<GroupProfile> = Vec::with_capacity(groups.len());
        for g in groups {
            let members = &g.member_indices;
            // Derive template from members[0] vs members[1] (or [0] alone).
            let a_raw = &lines[members[0]].message;
            let a_norm = norm.normalize_for_grouping(a_raw);
            let b_norm = if members.len() > 1 {
                norm.normalize_for_grouping(&lines[members[1]].message)
            } else {
                a_norm.clone()
            };
            let template = derive_pair_template(a_raw, &a_norm, &b_norm);
            let regex = regex_from_placeholder(&template);
            let slot_count = template.matches(crate::compress::template::PLACEHOLDER).count();

            // Capture raw slot strings per member.
            let mut raw_slots: Vec<Vec<Option<String>>> = Vec::with_capacity(members.len());
            // numeric_vals[slot] = list of parsed numeric values (present only)
            let mut numeric_vals: Vec<Vec<f64>> = vec![Vec::new(); slot_count];
            // present_count[slot] = number of members with a captured (non-None) value
            let mut present_count: Vec<usize> = vec![0; slot_count];

            for &m in members {
                let msg = &lines[m].message;
                let caps = capture_slots(regex.as_ref(), msg, slot_count);
                for (si, cap) in caps.iter().enumerate() {
                    if let Some(raw) = cap {
                        present_count[si] += 1;
                        if let Some(mm) = num_re().find(raw) {
                            if let Ok(val) = mm.as_str().parse::<f64>() {
                                numeric_vals[si].push(val);
                            }
                        }
                    }
                }
                raw_slots.push(caps);
            }

            // A slot is numeric iff >= 50% of *present* values parse as a number.
            let mut numeric: Vec<Option<SlotNumeric>> = Vec::with_capacity(slot_count);
            for si in 0..slot_count {
                let present = present_count[si];
                let parsed = numeric_vals[si].len();
                if present > 0 && parsed * 2 >= present && parsed > 0 {
                    let med = median(&numeric_vals[si]);
                    let mn = numeric_vals[si]
                        .iter()
                        .cloned()
                        .fold(f64::INFINITY, f64::min);
                    let mx = numeric_vals[si]
                        .iter()
                        .cloned()
                        .fold(f64::NEG_INFINITY, f64::max);
                    numeric.push(Some(SlotNumeric {
                        min: mn,
                        median: med,
                        max: mx,
                    }));
                } else {
                    numeric.push(None);
                }
            }

            profiles.push(GroupProfile {
                template,
                regex,
                slot_count,
                raw_slots,
                numeric,
            });
        }

        Profile {
            gid,
            profiles,
            outlier_factor,
        }
    }

    /// Port of `is_tail_outlier`: median>0 && (v > factor*median || v < median/factor).
    pub fn is_tail_outlier(&self, g_idx: usize, slot: usize, v: f64) -> bool {
        let prof = &self.profiles[g_idx];
        match prof.numeric.get(slot).and_then(|o| o.as_ref()) {
            Some(SlotNumeric { median, .. }) if *median > 0.0 => {
                v > self.outlier_factor * *median || v < *median / self.outlier_factor
            }
            _ => false,
        }
    }

    /// Parse the numeric value of a captured slot string (first numeric match).
    pub fn slot_numeric_value(s: &str) -> Option<f64> {
        num_re()
            .find(s)
            .and_then(|m| m.as_str().parse::<f64>().ok())
    }
}

/// Capture the `<*>` slot raw strings for a line. Returns a vector of length
/// `slot_count`; entries are None when the regex doesn't match. LibreLog comma /
/// whitespace normalization is applied before matching.
pub fn capture_slots(
    regex: Option<&Regex>,
    msg: &str,
    slot_count: usize,
) -> Vec<Option<String>> {
    let re = match regex {
        Some(r) => r,
        None => return vec![None; slot_count],
    };
    let prepared = msg.replace(',', "");
    let prepared = prepared.trim();
    match re.captures(prepared) {
        Some(caps) => (1..=slot_count)
            .map(|i| caps.get(i).map(|m| m.as_str().to_string()))
            .collect(),
        None => vec![None; slot_count],
    }
}

/// Build distinct first-seen samples (cap N) and a distinct count for a slot.
pub fn distinct_samples(values: &[Option<String>], cap: usize) -> (Vec<String>, usize) {
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut distinct = 0usize;
    for v in values.iter().flatten() {
        if seen.insert(v.clone(), ()).is_none() {
            distinct += 1;
            if order.len() < cap {
                order.push(v.clone());
            }
        }
    }
    (order, distinct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::grouper::{Grouper, StructuralExactGrouper};
    use crate::compress::guard::Guard;

    fn lines_of(msgs: &[&str]) -> Vec<LogLine> {
        msgs.iter().map(|m| LogLine::new(m.to_string())).collect()
    }

    #[test]
    fn median_odd_and_even() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
    }

    #[test]
    fn median_over_all_values_not_distinct() {
        // The key correctness check from the spec: median of [20,20,20,8400]
        // must be ~20 (over ALL values), NOT (20+8400)/2 = 4210 (distinct).
        let m = median(&[20.0, 20.0, 20.0, 8400.0]);
        assert!((m - 20.0).abs() < 1e-9, "median was {m}");
    }

    #[test]
    fn outlier_at_boundary() {
        let lines = lines_of(&[
            "latency ms 20",
            "latency ms 20",
            "latency ms 20",
            "latency ms 8400",
        ]);
        let norm = Normalizer::new();
        let groups = StructuralExactGrouper.group(&lines, &norm, &Guard::new());
        let p = Profile::build(&lines, &groups, &norm, 4.0);
        // group 0, slot 0, median 20.
        // v=8400 > 4*20=80 -> outlier
        assert!(p.is_tail_outlier(0, 0, 8400.0));
        // v=20 not outlier
        assert!(!p.is_tail_outlier(0, 0, 20.0));
        // boundary: exactly 4*median = 80 is NOT > 80 -> not outlier
        assert!(!p.is_tail_outlier(0, 0, 80.0));
        // just above
        assert!(p.is_tail_outlier(0, 0, 80.5));
        // low boundary: median/4 = 5; exactly 5 not < 5 -> not outlier; 4.9 is
        assert!(!p.is_tail_outlier(0, 0, 5.0));
        assert!(p.is_tail_outlier(0, 0, 4.9));
    }

    #[test]
    fn numeric_detection_majority() {
        // slot mostly numeric -> numeric profile
        let lines = lines_of(&["count is 1", "count is 2", "count is 3"]);
        let norm = Normalizer::new();
        let groups = StructuralExactGrouper.group(&lines, &norm, &Guard::new());
        let p = Profile::build(&lines, &groups, &norm, 4.0);
        assert!(p.profiles[0].numeric[0].is_some());
    }
}
