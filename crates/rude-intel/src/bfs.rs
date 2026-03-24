//! Generic BFS traversal on the call graph.
//!
//! Provides `bfs_generic`, `BfsDirection`, and `HasIdx` —
//! the building blocks for context, impact, and trace modules.

use std::collections::VecDeque;

use crate::graph::CallGraph;

// ── Shared trait for BFS entry types ─────────────────────────────────────

/// Trait for BFS entry types that carry a graph index.
pub trait HasIdx {
    fn idx(&self) -> u32;
}

// ── Generic BFS ──────────────────────────────────────────────────────────

/// Adjacency selector for BFS direction.
pub enum BfsDirection {
    /// Follow callees (forward traversal).
    Forward,
    /// Follow callers (reverse traversal).
    Reverse,
}

/// Run a depth-limited BFS on the call graph.
///
/// The `direction` parameter selects which adjacency list to follow.
/// For each visited node the `make_entry` callback produces the result entry;
/// returning `None` skips the node (useful for test filtering) but still
/// continues BFS through its neighbours.
pub fn bfs_generic<T>(
    graph: &CallGraph,
    seeds: &[u32],
    max_depth: u32,
    direction: BfsDirection,
    mut make_entry: impl FnMut(u32, u32) -> Option<T>,
) -> Vec<T> {
    let mut visited = vec![false; graph.len()];
    let mut queue: VecDeque<(u32, u32)> = VecDeque::new();
    let mut results = Vec::new();

    for &seed in seeds {
        if (seed as usize) < graph.len() && !visited[seed as usize] {
            visited[seed as usize] = true;
            queue.push_back((seed, 0));
        }
    }

    while let Some((idx, depth)) = queue.pop_front() {
        if let Some(entry) = make_entry(idx, depth) {
            results.push(entry);
        }

        if depth < max_depth {
            let neighbours = match direction {
                BfsDirection::Forward => &graph.callees[idx as usize],
                BfsDirection::Reverse => &graph.callers[idx as usize],
            };
            for &next in neighbours {
                if !visited[next as usize] {
                    visited[next as usize] = true;
                    queue.push_back((next, depth + 1));
                }
            }
        }
    }

    results
}
