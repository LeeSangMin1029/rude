use anyhow::Result;

use rude_intel::graph::build::CallGraph;
use super::common::{load_or_build_graph, resolve_symbol, fmt_scope, print_tagged, TaggedEntry};
use super::blast::run_blast;

fn disambiguate(graph: &CallGraph, idx: usize) -> String {
    let chunk = &graph.chunks[idx];
    let dn = chunk.dn();
    if dn.contains("::") {
        return dn.to_string();
    }
    if let Some(sig) = &chunk.signature {
        if let Some(start) = sig.find("impl ") {
            let after = &sig[start + 5..];
            if let Some(end) = after.find(">::") {
                let impl_type = after[..end].rsplit("::").next().unwrap_or(&after[..end]);
                let for_part = after[..end].rsplit(" for ").next().unwrap_or(impl_type);
                let for_short = for_part.rsplit("::").next().unwrap_or(for_part);
                return format!("{for_short}::{dn}");
            }
        }
    }
    let name = &chunk.name;
    if name.contains("::") {
        return rude_util::display_symbol_name(name);
    }
    dn.to_string()
}

pub fn run_context(
    symbol: String,
    depth: u32,
    source: bool,
    include_tests: bool,
    scope: Option<String>,
    tree: bool,
    blast: bool,
) -> Result<()> {
    if tree { return run_context_tree(&symbol, depth, include_tests); }
    if blast { return run_blast(&symbol, depth, include_tests, &scope); }

    use rude_intel::context_cmd;
    let graph = load_or_build_graph()?;
    let result = context_cmd::build_context(&graph, &symbol, depth);

    if result.seeds.is_empty() {
        println!("No symbol found matching \"{symbol}\".");
        return Ok(());
    }

    let seed0 = result.seeds.first().copied();
    let site = |a, b| seed0.map_or(0, |_| graph.call_site_line(a, b));
    let mut entries: Vec<TaggedEntry> = Vec::new();
    for &i in &result.seeds  { entries.push(TaggedEntry { idx: i, tag: "def",    sig: true,  call_line: 0 }); }
    for e in &result.callers { entries.push(TaggedEntry { idx: e.idx, tag: "caller", sig: false, call_line: site(e.idx, seed0.unwrap_or(0)) }); }
    for e in &result.callees { entries.push(TaggedEntry { idx: e.idx, tag: "callee", sig: false, call_line: site(seed0.unwrap_or(0), e.idx) }); }
    for &i in &result.types  { entries.push(TaggedEntry { idx: i, tag: "type",   sig: false, call_line: 0 }); }
    if include_tests {
        for &i in &result.tests { entries.push(TaggedEntry { idx: i, tag: "test", sig: false, call_line: site(i, seed0.unwrap_or(0)) }); }
    }

    if let Some(ref sc) = scope {
        entries.retain(|e| e.tag == "def" || {
            let f = &graph.chunks[e.idx as usize].file;
            f.starts_with(sc.as_str()) || f.contains(sc.as_str())
        });
    }

    let (alias_map, _) = graph.global_aliases();

    if !result.impl_groups.is_empty() {
        let trait_label = result.impl_groups.first()
            .filter(|g| !g.trait_name.is_empty())
            .map(|g| format!("{}::", g.trait_name))
            .unwrap_or_default();
        println!("=== context: {trait_label}{symbol} ({} impls) ===\n", result.impl_groups.len());
        for g in &result.impl_groups {
            let i = g.seed_idx as usize;
            let file = rude_util::relative_path(&graph.chunks[i].file);
            let loc = rude_util::format_lines_opt(graph.chunks[i].lines);
            println!("  @ {}{loc}  {}", rude_util::apply_alias(file, &alias_map), g.impl_name);
            if let Some(s) = &graph.chunks[i].signature {
                if !s.is_empty() { println!("    {}", rude_util::shorten_signature(s, 100)); }
            }
            for c in &g.callers {
                let ci = c.idx as usize;
                let file = rude_util::apply_alias(rude_util::relative_path(&graph.chunks[ci].file), &alias_map);
                println!("    [caller] {file}{} {}", rude_util::format_lines_opt(graph.chunks[ci].lines), disambiguate(&graph, ci));
            }
            for c in &g.callees {
                let ci = c.idx as usize;
                let file = rude_util::apply_alias(rude_util::relative_path(&graph.chunks[ci].file), &alias_map);
                println!("    [callee] {file}{} {}", rude_util::format_lines_opt(graph.chunks[ci].lines), disambiguate(&graph, ci));
            }
            println!();
        }
    } else {
        println!("=== context: {symbol}{} ({} caller, {} callee, {} type, {} test) ===\n",
            fmt_scope(&scope), result.callers.len(), result.callees.len(),
            result.types.len(), result.tests.len());
        print_tagged(&graph, &entries, source, &alias_map);
    }

    if !include_tests && !result.tests.is_empty() {
        println!("  {} tests (use --include-tests to show)\n", result.tests.len());
    }

    if let Some(&si) = result.seeds.iter().find(|&&s| graph.chunks[s as usize].kind == "struct") {
        let field_entries = graph.find_field_accesses_for_type(&graph.chunks[si as usize].name.clone());
        if !field_entries.is_empty() {
            println!("@ [field accesses]");
            for (field, indices) in &field_entries {
                let names: Vec<&str> = indices.iter().map(|&i| graph.chunks[i as usize].dn()).collect();
                println!("  .{field} ← {}", names.join(", "));
            }
            println!();
        }
    }
    Ok(())
}

fn run_context_tree(symbol: &str, depth: u32, include_tests: bool) -> Result<()> {
    use rude_intel::jump;
    let graph = load_or_build_graph()?;
    let Some(seeds) = resolve_symbol(&graph, symbol) else { return Ok(()) };
    let (alias_map, _) = graph.global_aliases();
    println!("=== jump: {symbol} ===\n");
    print!("{}", jump::render_tree(&graph, &jump::build_flow_tree(&graph, &seeds, depth, !include_tests), &alias_map));
    Ok(())
}
