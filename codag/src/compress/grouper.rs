//! Groupers.
//!
//! Three deterministic groupers, ported / adapted from
//! `v2/src/v2/templater/grouping.py`:
//!   - `StructuralExactGrouper`  — exact key on the structural skeleton (default).
//!   - `FixedDepthTree`          — length -> prefix -> Jaccard merge w/ guard.
//!   - `GuardedAdaptiveGrouper`  — structural groups + relaxed union-find merge.
//!
//! Determinism is mandatory: groups are emitted in ascending first-member-index
//! order, members ascending. We never iterate a `HashMap` for emission; blocks
//! are keyed in a `BTreeMap`, and the adaptive union-find uses path-halving with
//! index-ordered processing.

use std::collections::BTreeMap;

use crate::compress::guard::Guard;
use crate::compress::normalize::{jaccard, Normalizer};
use crate::compress::template::{
    derive_pair_template, is_useful_template, regex_from_placeholder, template_matches_line,
};
use crate::compress::{GrouperKind, LogLine, DEFAULT_MIN_STATIC_CHARS};

/// One group of similar lines. `member_indices` is ascending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub member_indices: Vec<usize>,
}

/// A grouper assigns each line to a group.
pub trait Grouper {
    fn group(&self, lines: &[LogLine], norm: &Normalizer, guard: &Guard) -> Vec<Group>;
}

/// Build a `Box<dyn Grouper>` from the config kind.
pub fn make_grouper(kind: GrouperKind, prefix_len: usize, jaccard_th: f64) -> Box<dyn Grouper> {
    match kind {
        GrouperKind::Structural => Box::new(StructuralExactGrouper),
        GrouperKind::Fixed => Box::new(FixedDepthTree {
            prefix_len,
            jaccard_th,
        }),
        GrouperKind::Adaptive => Box::new(GuardedAdaptiveGrouper {
            max_diff: 2,
            sample: 64,
        }),
    }
}

/// Emit groups sorted by their smallest member index, members ascending.
fn finalize(mut buckets: Vec<Vec<usize>>) -> Vec<Group> {
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

// ---------------------------------------------------------------------------
// StructuralExactGrouper
// ---------------------------------------------------------------------------

/// Exact grouping on the aggressive structural skeleton. Lines with an identical
/// skeleton join the same group. Port of `StructuralExactGrouper`.
#[derive(Debug, Default, Clone)]
pub struct StructuralExactGrouper;

impl Grouper for StructuralExactGrouper {
    fn group(&self, lines: &[LogLine], norm: &Normalizer, _guard: &Guard) -> Vec<Group> {
        // Map skeleton -> bucket index, preserving first-seen order via the
        // index. We collect into a Vec<Vec<usize>> then finalize.
        let mut by_key: BTreeMap<Vec<String>, usize> = BTreeMap::new();
        let mut buckets: Vec<Vec<usize>> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let key = norm.structural_normalize_for_grouping(&line.message);
            let bidx = *by_key.entry(key).or_insert_with(|| {
                buckets.push(Vec::new());
                buckets.len() - 1
            });
            buckets[bidx].push(i);
        }
        finalize(buckets)
    }
}

/// Expose the structural skeletons keyed per bucket (used by the adaptive
/// grouper). Returns `(buckets, skeletons)` where `skeletons[k]` is the skeleton
/// of bucket `k`'s first member.
fn structural_buckets(
    lines: &[LogLine],
    norm: &Normalizer,
) -> (Vec<Vec<usize>>, Vec<Vec<String>>) {
    let mut by_key: BTreeMap<Vec<String>, usize> = BTreeMap::new();
    let mut buckets: Vec<Vec<usize>> = Vec::new();
    let mut skeletons: Vec<Vec<String>> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let key = norm.structural_normalize_for_grouping(&line.message);
        let bidx = *by_key.entry(key.clone()).or_insert_with(|| {
            buckets.push(Vec::new());
            skeletons.push(key.clone());
            buckets.len() - 1
        });
        buckets[bidx].push(i);
    }
    (buckets, skeletons)
}

// ---------------------------------------------------------------------------
// FixedDepthTree
// ---------------------------------------------------------------------------

/// Length -> prefix(prefix_len) -> Jaccard merge tree. Port of `FixedDepthTree`,
/// using `normalize_for_grouping`'s masking (the production grouper masks
/// numerics/paths before similarity). Refuses a merge if the guard finds a bad
/// static conflict against the group's canonical (first) member.
#[derive(Debug, Clone)]
pub struct FixedDepthTree {
    pub prefix_len: usize,
    pub jaccard_th: f64,
}

impl Grouper for FixedDepthTree {
    fn group(&self, lines: &[LogLine], norm: &Normalizer, guard: &Guard) -> Vec<Group> {
        // (length, prefix) -> list of (canonical masked tokens, member indices)
        type Bucket = Vec<(Vec<String>, Vec<usize>)>;
        let mut tree: BTreeMap<(usize, Vec<String>), Bucket> = BTreeMap::new();

        for (i, line) in lines.iter().enumerate() {
            let masked = norm.normalize_for_grouping(&line.message);
            let length = masked.len();
            let prefix: Vec<String> = masked.iter().take(self.prefix_len).cloned().collect();
            let bucket = tree.entry((length, prefix)).or_default();

            let mut placed = false;
            for (canon, members) in bucket.iter_mut() {
                if jaccard(&masked, canon) >= self.jaccard_th
                    && !guard.has_bad_static_conflict(&masked, canon)
                {
                    members.push(i);
                    placed = true;
                    break;
                }
            }
            if !placed {
                bucket.push((masked, vec![i]));
            }
        }

        let mut buckets: Vec<Vec<usize>> = Vec::new();
        for (_k, groups) in tree {
            for (_canon, members) in groups {
                buckets.push(members);
            }
        }
        finalize(buckets)
    }
}

// ---------------------------------------------------------------------------
// GuardedAdaptiveGrouper
// ---------------------------------------------------------------------------

/// Structural-exact groups, then a deterministic union-find post-merge of groups
/// whose skeletons differ in <= `max_diff` positions, gated by:
///   1. relaxed semantic guard (`has_semantic_conflict`) — no antonyms / method /
///      status / length-delta conflict;
///   2. the derived pair-template is useful;
///   3. that template's regex matches sampled members of *both* groups.
#[derive(Debug, Clone)]
pub struct GuardedAdaptiveGrouper {
    pub max_diff: usize,
    pub sample: usize,
}

/// Union-find with path-halving.
struct UnionFind {
    parent: Vec<usize>,
}
impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind {
            parent: (0..n).collect(),
        }
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]]; // path-halving
            x = self.parent[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        // Deterministic: lower root wins.
        if ra < rb {
            self.parent[rb] = ra;
        } else {
            self.parent[ra] = rb;
        }
    }
}

/// Count positional skeleton differences over the overlapping prefix; if lengths
/// differ, that delta counts toward the difference total.
fn skeleton_diff(a: &[String], b: &[String]) -> usize {
    let min_len = a.len().min(b.len());
    let mut diff = a.len().abs_diff(b.len());
    for i in 0..min_len {
        if a[i] != b[i] {
            diff += 1;
        }
    }
    diff
}

impl GuardedAdaptiveGrouper {
    fn pair_template_validates(
        &self,
        lines: &[LogLine],
        members_a: &[usize],
        skel_a: &[String],
        members_b: &[usize],
        skel_b: &[String],
    ) -> bool {
        // Derive a pair template from the first members of each group.
        let a_raw = &lines[members_a[0]].message;
        let template = derive_pair_template(a_raw, skel_a, skel_b);
        if !is_useful_template(&template, DEFAULT_MIN_STATIC_CHARS) {
            return false;
        }
        let re = match regex_from_placeholder(&template) {
            Some(r) => r,
            None => return false,
        };
        // Must match sampled members of BOTH groups.
        let matches_all = |members: &[usize]| -> bool {
            members
                .iter()
                .take(self.sample)
                .all(|&idx| template_matches_line(&re, &lines[idx].message))
        };
        matches_all(members_a) && matches_all(members_b)
    }
}

impl Grouper for GuardedAdaptiveGrouper {
    fn group(&self, lines: &[LogLine], norm: &Normalizer, guard: &Guard) -> Vec<Group> {
        let (buckets, skeletons) = structural_buckets(lines, norm);
        let n = buckets.len();
        if n <= 1 {
            return finalize(buckets);
        }
        let mut uf = UnionFind::new(n);

        // Deterministic O(n^2) pairwise scan in index order.
        for i in 0..n {
            for j in (i + 1)..n {
                // Skip if already unioned.
                if uf.find(i) == uf.find(j) {
                    continue;
                }
                let skel_a = &skeletons[i];
                let skel_b = &skeletons[j];
                if skeleton_diff(skel_a, skel_b) > self.max_diff {
                    continue;
                }
                if guard.has_semantic_conflict(skel_a, skel_b) {
                    continue;
                }
                if !self.pair_template_validates(
                    lines,
                    &buckets[i],
                    skel_a,
                    &buckets[j],
                    skel_b,
                ) {
                    continue;
                }
                uf.union(i, j);
            }
        }

        // Collect merged buckets by root, preserving determinism.
        let mut merged: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (b, members) in buckets.iter().enumerate() {
            let root = uf.find(b);
            merged.entry(root).or_default().extend(members.iter());
        }
        let combined: Vec<Vec<usize>> = merged.into_values().collect();
        finalize(combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(msgs: &[&str]) -> Vec<LogLine> {
        msgs.iter().map(|m| LogLine::new(m.to_string())).collect()
    }

    #[test]
    fn structural_exact_splits_distinct_shapes() {
        let lines = lines_of(&[
            "acquired connection from pool",
            "released connection to pool",
            "acquired connection from pool",
        ]);
        let g = StructuralExactGrouper;
        let groups = g.group(&lines, &Normalizer::new(), &Guard::new());
        assert_eq!(groups.len(), 2);
        // first group: acquired (indices 0,2)
        assert_eq!(groups[0].member_indices, vec![0, 2]);
        assert_eq!(groups[1].member_indices, vec![1]);
    }

    #[test]
    fn member_indices_ascending() {
        let lines = lines_of(&["x same", "x same", "x same"]);
        let g = StructuralExactGrouper;
        let groups = g.group(&lines, &Normalizer::new(), &Guard::new());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, vec![0, 1, 2]);
    }

    #[test]
    fn adaptive_defrags_worker_name_style() {
        // Two structural groups differing only by a static worker name. The
        // structural skeleton keeps the static word, so these are 2 groups; the
        // relaxed adaptive merge should union them (generic-alpha not blocked).
        let lines = lines_of(&[
            "started background worker alpha now",
            "started background worker bravo now",
        ]);
        let norm = Normalizer::new();
        let guard = Guard::new();
        // structural: 2 groups
        let s = StructuralExactGrouper.group(&lines, &norm, &guard);
        assert_eq!(s.len(), 2);
        // adaptive: merged into 1
        let a = GuardedAdaptiveGrouper {
            max_diff: 2,
            sample: 64,
        }
        .group(&lines, &norm, &guard);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].member_indices, vec![0, 1]);
    }

    #[test]
    fn adaptive_refuses_antonym_merge() {
        let lines = lines_of(&[
            "health check task succeeded ok",
            "health check task failed now",
        ]);
        let norm = Normalizer::new();
        let guard = Guard::new();
        let a = GuardedAdaptiveGrouper {
            max_diff: 2,
            sample: 64,
        }
        .group(&lines, &norm, &guard);
        // antonym succeeded vs failed -> refused; 2 groups
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn fixed_depth_groups_numeric_variation() {
        let lines = lines_of(&[
            "pool acquire in_use 5",
            "pool acquire in_use 6",
            "pool acquire in_use 7",
        ]);
        let g = FixedDepthTree {
            prefix_len: 3,
            jaccard_th: 0.5,
        };
        let groups = g.group(&lines, &Normalizer::new(), &Guard::new());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, vec![0, 1, 2]);
    }
}
