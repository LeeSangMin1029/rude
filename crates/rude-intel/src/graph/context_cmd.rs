//! Unified context gathering for a symbol.
//! Collects definition, callers, callees, related types, and tests in one pass.

use crate::graph::bfs::{bfs_generic, BfsDirection};
use crate::graph::build::CallGraph;
use crate::data::parse::ParsedChunk;

/// A single entry in the context result (definition, caller, callee, type, or test).
pub struct ContextEntry {
    pub idx: u32,
    pub depth: u32,
}

/// Complete context for a symbol: definition + callers + callees + types + tests.
pub struct ContextResult {
    /// Seed chunk indices (the symbol's own definitions).
    pub seeds: Vec<u32>,
    /// Callers found via reverse BFS (excludes seeds).
    pub callers: Vec<ContextEntry>,
    /// Callees found via forward BFS (excludes seeds).
    pub callees: Vec<ContextEntry>,
    /// Chunk indices of types referenced by the seed chunks.
    pub types: Vec<u32>,
    /// Chunk indices of test functions that call the symbol.
    pub tests: Vec<u32>,
    /// Unresolved call names from seed chunks (external/std dependencies).
    pub unresolved_calls: Vec<String>,
}

/// Build unified context for a symbol.
///
/// Resolves `symbol` via `graph.resolve()`, then collects callers (reverse BFS),
/// callees (forward BFS), referenced types, and test functions — all in one pass.
pub fn build_context(
    graph: &CallGraph,
    chunks: &[ParsedChunk],
    symbol: &str,
    depth: u32,
) -> ContextResult {
    let seeds = graph.resolve(symbol);
    let seeds = crate::impact::expand_seeds_with_traits(graph, &seeds);

    // Callers: reverse BFS, exclude seeds and test chunks (tests shown separately).
    let callers = {
        let all = bfs_generic(graph, &seeds, depth, BfsDirection::Reverse, |idx, d| {
            Some(ContextEntry { idx, depth: d })
        });
        all.into_iter()
            .filter(|e| e.depth > 0 && !graph.is_test[e.idx as usize])
            .collect()
    };

    // Callees: forward BFS, exclude seeds themselves.
    let callees = {
        let all = bfs_generic(graph, &seeds, depth, BfsDirection::Forward, |idx, d| {
            Some(ContextEntry { idx, depth: d })
        });
        all.into_iter().filter(|e| e.depth > 0).collect()
    };

    // Types: collect type names from seed chunks' `types` field, resolve to chunk indices.
    let types = collect_types(graph, chunks, &seeds);

    // Tests: find test chunks that call the symbol.
    let tests = collect_tests(graph, &seeds, depth);

    // Unresolved calls: calls from seed chunks that don't map to any graph callee.
    let unresolved_calls = collect_unresolved(graph, chunks, &seeds);

    ContextResult { seeds, callers, callees, types, tests, unresolved_calls }
}

/// Resolve type names from seed chunks to chunk indices.
fn collect_types(graph: &CallGraph, chunks: &[ParsedChunk], seeds: &[u32]) -> Vec<u32> {
    let mut type_indices = Vec::new();
    let mut seen = vec![false; graph.len()];

    // Mark seeds as seen so they don't appear in types.
    for &s in seeds {
        if (s as usize) < seen.len() {
            seen[s as usize] = true;
        }
    }

    for &seed in seeds {
        let seed_usize = seed as usize;
        if seed_usize >= chunks.len() {
            continue;
        }
        let chunk = &chunks[seed_usize];
        for type_name in &chunk.types {
            let resolved = graph.resolve(type_name);
            for idx in resolved {
                let idx_usize = idx as usize;
                if idx_usize < seen.len() && !seen[idx_usize] {
                    seen[idx_usize] = true;
                    type_indices.push(idx);
                }
            }
        }
    }

    type_indices
}

/// Collect call names from seed chunks that couldn't be resolved to any graph node.
/// These are typically external crate / std library calls.
fn collect_unresolved(graph: &CallGraph, chunks: &[ParsedChunk], seeds: &[u32]) -> Vec<String> {
    let mut unresolved = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for &seed in seeds {
        let seed_usize = seed as usize;
        if seed_usize >= chunks.len() {
            continue;
        }
        // Get the set of resolved callee names for this seed.
        let resolved_names: std::collections::HashSet<&str> = graph.callees
            .get(seed_usize)
            .map(|cs| cs.iter().map(|&idx| graph.names[idx as usize].as_str()).collect())
            .unwrap_or_default();

        for call in &chunks[seed_usize].calls {
            // Skip if it resolves to a known callee (by short name match).
            // For `receiver.method` calls, use the leaf method name for matching.
            let short = call.rsplit("::").next().unwrap_or(call);
            let leaf = short.rsplit('.').next().unwrap_or(short);
            let is_resolved = resolved_names.iter().any(|n| {
                let n_short = n.rsplit("::").next().unwrap_or(n);
                n_short.eq_ignore_ascii_case(leaf)
            });
            if !is_resolved && !is_noise(call) && seen.insert(call.clone()) {
                unresolved.push(call.clone());
            }
        }
    }
    unresolved
}

/// Filter out common std/self method noise from unresolved calls.
pub(crate) fn is_noise(call: &str) -> bool {
    // self.field calls are usually internal method chains
    if call.starts_with("self.") { return true; }
    // Common std traits / methods that are never interesting
    let leaf = call.rsplit('.').next().unwrap_or(call);
    matches!(leaf,
        "clone" | "to_string" | "to_owned" | "to_vec" | "into"
        | "unwrap" | "expect" | "unwrap_or" | "unwrap_or_default" | "unwrap_or_else"
        | "map" | "map_err" | "and_then" | "or_else" | "ok" | "err"
        | "collect" | "iter" | "into_iter" | "push" | "pop" | "len" | "is_empty"
        | "as_ref" | "as_mut" | "borrow" | "deref"
        | "fmt" | "write" | "read" | "flush"
    ) || matches!(call, "Ok" | "Err" | "Some" | "None" | "format" | "println" | "eprintln" | "vec")
}

/// Find test chunks reachable via reverse BFS up to `depth` levels from seeds.
fn collect_tests(graph: &CallGraph, seeds: &[u32], depth: u32) -> Vec<u32> {
    let all = bfs_generic(graph, seeds, depth, BfsDirection::Reverse, |idx, d| {
        if d > 0 && graph.is_test[idx as usize] {
            Some(idx)
        } else {
            None
        }
    });
    all
}
