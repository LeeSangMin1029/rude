use crate::graph::bfs::{bfs_generic, BfsDirection};
use crate::graph::build::CallGraph;

pub struct ContextEntry {
    pub idx: u32,
    pub depth: u32,
}

pub struct ImplGroup {
    pub trait_name: String,
    pub impl_name: String,
    pub seed_idx: u32,
    pub callers: Vec<ContextEntry>,
    pub callees: Vec<ContextEntry>,
}

pub struct ContextResult {
    pub seeds: Vec<u32>,
    pub callers: Vec<ContextEntry>,
    pub callees: Vec<ContextEntry>,
    pub types: Vec<u32>,
    pub tests: Vec<u32>,
    pub unresolved_calls: Vec<String>,
    pub impl_groups: Vec<ImplGroup>,
}

pub fn build_context(
    graph: &CallGraph,
    symbol: &str,
    depth: u32,
) -> ContextResult {
    let seeds = graph.resolve(symbol);
    let seeds = crate::impact::expand_seeds_with_traits(graph, &seeds);

    let callers = {
        let all = bfs_generic(graph, &seeds, depth, BfsDirection::Reverse, |idx, d| {
            Some(ContextEntry { idx, depth: d })
        });
        all.into_iter()
            .filter(|e| e.depth > 0 && !graph.is_test[e.idx as usize])
            .collect()
    };

    let callees = {
        let all = bfs_generic(graph, &seeds, depth, BfsDirection::Forward, |idx, d| {
            Some(ContextEntry { idx, depth: d })
        });
        all.into_iter()
            .filter(|e| e.depth > 0 && !is_derived_noise(&graph.chunks[e.idx as usize].name))
            .collect()
    };

    let types = collect_types(graph, &seeds);
    let tests = collect_tests(graph, &seeds, depth);
    let unresolved_calls = collect_unresolved(graph, &seeds);
    let impl_groups = if seeds.len() > 1 {
        build_impl_groups(graph, &seeds, depth)
    } else {
        Vec::new()
    };

    ContextResult { seeds, callers, callees, types, tests, unresolved_calls, impl_groups }
}

fn collect_types(graph: &CallGraph, seeds: &[u32]) -> Vec<u32> {
    let mut type_indices = Vec::new();
    let mut seen = vec![false; graph.len()];

    for &s in seeds {
        if (s as usize) < seen.len() {
            seen[s as usize] = true;
        }
    }

    for &seed in seeds {
        let seed_usize = seed as usize;
        if seed_usize >= graph.chunks.len() {
            continue;
        }
        let chunk = &graph.chunks[seed_usize];
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

fn collect_unresolved(graph: &CallGraph, seeds: &[u32]) -> Vec<String> {
    let mut unresolved = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for &seed in seeds {
        let seed_usize = seed as usize;
        if seed_usize >= graph.chunks.len() {
            continue;
        }
        let resolved_names: std::collections::HashSet<&str> = graph.callees
            .get(seed_usize)
            .map(|cs| cs.iter().map(|&idx| graph.chunks[idx as usize].name.as_str()).collect())
            .unwrap_or_default();

        for call in &graph.chunks[seed_usize].calls {
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

pub(crate) fn is_derived_noise(name: &str) -> bool {
    if !name.starts_with('<') { return false; }
    let Some(trait_part) = name.split("as ").nth(1) else { return false };
    let trait_path = trait_part.split('>').next().unwrap_or("");
    let trait_leaf = trait_path.rsplit("::").next().unwrap_or("");
    matches!(trait_leaf,
        "PartialEq" | "Eq" | "PartialOrd" | "Ord"
        | "Clone" | "Debug" | "Hash" | "Default"
        | "Serialize" | "Deserialize"
    )
}

pub(crate) fn is_noise(call: &str) -> bool {
    if call.starts_with("self.") { return true; }
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

fn build_impl_groups(graph: &CallGraph, seeds: &[u32], depth: u32) -> Vec<ImplGroup> {
    let mut groups = Vec::new();
    for &seed in seeds {
        let i = seed as usize;
        if i >= graph.chunks.len() { continue; }
        let chunk = &graph.chunks[i];
        let trait_name = graph.impl_of_trait.get(i)
            .and_then(|&t| t.map(|ti| graph.chunks[ti as usize].dn().to_string()))
            .unwrap_or_default();
        let impl_name = chunk.dn().to_string();
        let callers: Vec<ContextEntry> = graph.callers.get(i)
            .map(|cs| cs.iter()
                .filter(|&&c| !graph.is_test[c as usize])
                .map(|&c| ContextEntry { idx: c, depth: 1 })
                .collect())
            .unwrap_or_default();
        let callees: Vec<ContextEntry> = graph.callees.get(i)
            .map(|cs| cs.iter()
                .filter(|&&c| !is_derived_noise(&graph.chunks[c as usize].name))
                .map(|&c| ContextEntry { idx: c, depth: 1 })
                .collect())
            .unwrap_or_default();
        groups.push(ImplGroup { trait_name, impl_name, seed_idx: seed, callers, callees });
    }
    groups
}

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
