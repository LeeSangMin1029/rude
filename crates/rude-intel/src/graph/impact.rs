use crate::graph::bfs::{bfs_generic, BfsDirection, HasIdx};
use crate::graph::build::CallGraph;

pub struct BfsEntry {
    pub idx: u32,
    pub depth: u32,
    pub is_test: bool,
}

impl HasIdx for BfsEntry {
    fn idx(&self) -> u32 { self.idx }
}

pub fn bfs_reverse(graph: &CallGraph, seeds: &[u32], max_depth: u32) -> Vec<BfsEntry> {
    bfs_generic(graph, seeds, max_depth, BfsDirection::Reverse, |idx, depth| {
        Some(BfsEntry {
            idx,
            depth,
            is_test: graph.is_test[idx as usize],
        })
    })
}

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
