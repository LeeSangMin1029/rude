use anyhow::Result;

use rude_intel::graph;
use rude_intel::impact;

use super::common::{load_or_build_graph, resolve_symbol, fmt_scope, print_tagged, TaggedEntry};

pub(super) fn run_blast(symbol: &str, depth: u32, include_tests: bool, scope: &Option<String>) -> Result<()> {
    let effective_depth = if depth == 1 { 2 } else { depth };
    let graph = load_or_build_graph()?;
    let (alias_map, _) = graph.global_aliases();

    if let Some(dot) = symbol.find('.') {
        let key = format!("{}::{}", symbol[..dot].to_lowercase(), symbol[dot+1..].to_lowercase());
        let field_chunks = graph.find_field_access(&key);
        if field_chunks.is_empty() { println!("No field accesses found for {symbol}"); return Ok(()); }
        let all = impact::bfs_reverse(&graph, &field_chunks, effective_depth);
        let (prod, test) = count_prod_test(&all);
        print!("=== context: {symbol}{} ({} field accessors, {} affected, {} prod, {} test) ===\n",
            fmt_scope(scope), field_chunks.len(), prod + test, prod, test);
        let entries = tag_blast(all, &graph, scope, include_tests, Some(&field_chunks));
        print_tagged(&graph, &entries, false, &alias_map);
        return Ok(());
    }

    let Some(s) = resolve_symbol(&graph, symbol) else { return Ok(()) };

    let is_type = s.iter().any(|&i| matches!(graph.chunks[i as usize].kind.as_str(), "struct" | "enum"));
    if is_type {
        return run_type_blast(symbol, &graph, &s, effective_depth, include_tests, scope, &alias_map);
    }

    let seeds = impact::expand_seeds_with_traits(&graph, &s);
    let all = impact::bfs_reverse(&graph, &seeds, effective_depth);
    let (prod, test) = count_prod_test(&all);
    print!("=== context: {symbol}{} ({} affected, {} prod, {} test) ===\n",
        fmt_scope(scope), prod + test, prod, test);
    let entries = tag_blast(all, &graph, scope, include_tests, None);
    print_tagged(&graph, &entries, false, &alias_map);
    Ok(())
}

fn count_prod_test(entries: &[impact::BfsEntry]) -> (usize, usize) {
    entries.iter().filter(|e| e.depth > 0)
        .fold((0, 0), |(p, t), e| if e.is_test { (p, t + 1) } else { (p + 1, t) })
}

fn tag_blast(
    all: Vec<impact::BfsEntry>,
    graph: &graph::CallGraph,
    scope: &Option<String>,
    include_tests: bool,
    field_chunks: Option<&[u32]>,
) -> Vec<TaggedEntry> {
    all.into_iter()
        .filter(|e| scope.as_ref().map_or(true, |prefix| {
            let f = &graph.chunks[e.idx as usize].file;
            f.starts_with(prefix.as_str()) || f.contains(prefix.as_str())
        }))
        .filter(|e| include_tests || !e.is_test)
        .map(|e| {
            let tag = field_chunks.map_or_else(
                || depth_tag(e.depth),
                |fc| if fc.contains(&e.idx) { "field" } else { depth_tag(e.depth) },
            );
            TaggedEntry { idx: e.idx, tag, sig: false, call_line: 0 }
        })
        .collect()
}

fn run_type_blast(
    symbol: &str,
    graph: &graph::CallGraph,
    seeds: &[u32],
    depth: u32,
    include_tests: bool,
    scope: &Option<String>,
    alias_map: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let type_name = &graph.chunks[seeds[0] as usize].name;
    let field_entries = graph.find_field_accesses_for_type(type_name);
    let mut accessor_set = std::collections::HashSet::<u32>::new();
    for (_, indices) in &field_entries {
        accessor_set.extend(indices.iter().copied());
    }
    let accessors: Vec<u32> = accessor_set.iter().copied().collect();
    if accessors.is_empty() {
        println!("=== context: {symbol}{} (0 affected, 0 prod, 0 test) ===", fmt_scope(scope));
        print_tagged(graph, &[TaggedEntry { idx: seeds[0], tag: "target", sig: false, call_line: 0 }], false, alias_map);
        return Ok(());
    }
    let all = impact::bfs_reverse(graph, &accessors, depth);
    let (prod, test) = count_prod_test(&all);
    let total = prod + test;
    print!("=== context: {symbol}{} ({} field accessors, {} affected, {} prod, {} test) ===\n",
        fmt_scope(scope), accessors.len(), total, prod, test);
    let mut entries: Vec<TaggedEntry> = Vec::new();
    for &i in seeds {
        entries.push(TaggedEntry { idx: i, tag: "target", sig: false, call_line: 0 });
    }
    for e in all {
        if seeds.contains(&e.idx) { continue; }
        if !include_tests && e.is_test { continue; }
        if let Some(prefix) = scope {
            let f = &graph.chunks[e.idx as usize].file;
            if !f.starts_with(prefix.as_str()) && !f.contains(prefix.as_str()) { continue; }
        }
        let tag = if accessor_set.contains(&e.idx) { "field" } else { depth_tag(e.depth) };
        entries.push(TaggedEntry { idx: e.idx, tag, sig: false, call_line: 0 });
    }
    print_tagged(graph, &entries, false, alias_map);
    Ok(())
}

#[inline]
fn depth_tag(depth: u32) -> &'static str {
    match depth { 0 => "target", 1 => "d1", 2 => "d2", _ => "d3+" }
}
