use std::collections::VecDeque;

use crate::graph::build::CallGraph;

pub trait HasIdx {
    fn idx(&self) -> u32;
}

pub enum BfsDirection {
    Forward,
    Reverse,
}

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
