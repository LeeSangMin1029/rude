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

#[inline]
fn depth_tag(depth: u32) -> &'static str {
    match depth { 0 => "target", 1 => "d1", 2 => "d2", _ => "d3+" }
}
