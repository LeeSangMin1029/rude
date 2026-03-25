
use std::collections::VecDeque;

use crate::graph::build::CallGraph;

pub fn bfs_shortest_path(graph: &CallGraph, sources: &[u32], targets: &[u32]) -> Option<Vec<u32>> {
    let len = graph.len();
    let mut visited = vec![false; len];
    let mut parent: Vec<Option<u32>> = vec![None; len];
    let mut queue: VecDeque<u32> = VecDeque::new();

    // Mark all targets for O(1) lookup.
    let mut is_target = vec![false; len];
    for &t in targets {
        if (t as usize) < len {
            is_target[t as usize] = true;
        }
    }

    // Seed with sources.
    for &s in sources {
        if (s as usize) < len && !visited[s as usize] {
            visited[s as usize] = true;
            queue.push_back(s);
        }
    }

    // BFS through callees + callers (undirected traversal).
    while let Some(idx) = queue.pop_front() {
        if is_target[idx as usize] {
            // Reconstruct path.
            let mut path = vec![idx];
            let mut current = idx;
            while let Some(p) = parent[current as usize] {
                path.push(p);
                current = p;
            }
            path.reverse();
            return Some(path);
        }

        let i = idx as usize;
        let neighbors = graph.callees[i].iter().chain(graph.callers[i].iter());
        for &next in neighbors {
            if !visited[next as usize] {
                visited[next as usize] = true;
                parent[next as usize] = Some(idx);
                queue.push_back(next);
            }
        }
    }

    None
}
