//! Duplicate pair analysis — callee/caller Jaccard, blast radius, merge safety.
//!
//! Given duplicate function pairs from clone detection, analyzes each pair's
//! call graph relationships and blast radius to determine merge safety.

use std::collections::HashSet;

use crate::graph::CallGraph;
use crate::impact;

/// Analysis result for a single duplicate pair.
pub struct DupeAnalysis {
    pub idx_a: u32,
    pub idx_b: u32,
    pub callee_match_pct: f32,
    pub caller_match_pct: f32,
    pub blast_total: usize,
    pub blast_prod: usize,
    pub blast_test: usize,
    pub verdict: Verdict,
}

/// Merge safety verdict.
#[derive(Clone, Copy)]
pub enum Verdict {
    SafeToMerge,
    ReviewNeeded,
    DifferentLogic,
}

impl Verdict {
    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::SafeToMerge => "SAFE TO MERGE",
            Self::ReviewNeeded => "REVIEW NEEDED",
            Self::DifferentLogic => "DIFFERENT LOGIC",
        }
    }
}

/// Analyze a single duplicate pair for merge safety.
pub fn analyze_pair(graph: &CallGraph, idx_a: u32, idx_b: u32) -> DupeAnalysis {
    let callees_a: HashSet<u32> = graph.callees.get(idx_a as usize)
        .map(|v| v.iter().copied().collect())
        .unwrap_or_default();
    let callees_b: HashSet<u32> = graph.callees.get(idx_b as usize)
        .map(|v| v.iter().copied().collect())
        .unwrap_or_default();

    let callers_a: HashSet<u32> = graph.callers.get(idx_a as usize)
        .map(|v| v.iter().copied().collect())
        .unwrap_or_default();
    let callers_b: HashSet<u32> = graph.callers.get(idx_b as usize)
        .map(|v| v.iter().copied().collect())
        .unwrap_or_default();

    let callee_match_pct = jaccard(&callees_a, &callees_b);
    let caller_match_pct = jaccard(&callers_a, &callers_b);

    // Blast radius: BFS reverse depth=2 for both, then union.
    let entries_a = impact::bfs_reverse(graph, &[idx_a], 2);
    let entries_b = impact::bfs_reverse(graph, &[idx_b], 2);

    let mut all_indices: HashSet<u32> = HashSet::new();
    for e in &entries_a {
        if e.depth > 0 {
            all_indices.insert(e.idx);
        }
    }
    for e in &entries_b {
        if e.depth > 0 {
            all_indices.insert(e.idx);
        }
    }

    // Count prod/test from union indices.
    let mut blast_prod = 0usize;
    let mut blast_test = 0usize;
    for &idx in &all_indices {
        if graph.is_test.get(idx as usize).copied().unwrap_or(false) {
            blast_test += 1;
        } else {
            blast_prod += 1;
        }
    }
    let blast_total = blast_prod + blast_test;

    let verdict = if (callee_match_pct - 1.0).abs() < f32::EPSILON {
        Verdict::SafeToMerge
    } else if callee_match_pct >= 0.5 {
        Verdict::ReviewNeeded
    } else {
        Verdict::DifferentLogic
    };

    DupeAnalysis {
        idx_a,
        idx_b,
        callee_match_pct,
        caller_match_pct,
        blast_total,
        blast_prod,
        blast_test,
        verdict,
    }
}

/// Analyze multiple duplicate pairs.
pub fn analyze_pairs(graph: &CallGraph, pairs: &[(u32, u32)]) -> Vec<DupeAnalysis> {
    pairs.iter()
        .map(|&(a, b)| analyze_pair(graph, a, b))
        .collect()
}

/// Jaccard similarity: |A ∩ B| / |A ∪ B|. Returns 1.0 if both sets are empty.
fn jaccard(a: &HashSet<u32>, b: &HashSet<u32>) -> f32 {
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        1.0
    } else {
        intersection as f32 / union as f32
    }
}
