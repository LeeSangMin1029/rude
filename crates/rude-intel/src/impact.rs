//! Reverse BFS (callers direction) from a symbol.
//!
//! Answers "if I change this symbol, what else is affected?"
//! Traverses the callers adjacency list up to a configurable depth.

use crate::bfs::{bfs_generic, BfsDirection, HasIdx};
use crate::graph::CallGraph;

/// BFS result entry with depth.
pub struct BfsEntry {
    pub idx: u32,
    pub depth: u32,
    pub is_test: bool,
}

impl HasIdx for BfsEntry {
    fn idx(&self) -> u32 { self.idx }
}

/// Run depth-limited BFS on the callers direction (reverse).
pub fn bfs_reverse(graph: &CallGraph, seeds: &[u32], max_depth: u32) -> Vec<BfsEntry> {
    bfs_generic(graph, seeds, max_depth, BfsDirection::Reverse, |idx, depth| {
        Some(BfsEntry {
            idx,
            depth,
            is_test: graph.is_test[idx as usize],
        })
    })
}

/// Expand seeds to include trait-related symbols.
///
/// - If seed is a trait: add all its concrete impls from `trait_impls`
/// - If seed is a trait impl: add the trait definition via `impl_of_trait`
pub fn expand_seeds_with_traits(graph: &CallGraph, seeds: &[u32]) -> Vec<u32> {
    let mut expanded: Vec<u32> = seeds.to_vec();
    let mut seen: std::collections::HashSet<u32> = seeds.iter().copied().collect();

    for &seed in seeds {
        let i = seed as usize;
        // seed가 trait → 모든 impl 추가
        if graph.kinds[i] == "trait" {
            for &impl_idx in &graph.trait_impls[i] {
                if seen.insert(impl_idx) {
                    expanded.push(impl_idx);
                }
            }
        }
        // seed가 impl → 해당 trait 추가
        if let Some(trait_idx) = graph.impl_of_trait[i] {
            if seen.insert(trait_idx) {
                expanded.push(trait_idx);
            }
        }
    }

    expanded
}
