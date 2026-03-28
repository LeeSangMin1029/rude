use std::collections::BTreeMap;


use rude_intel::graph;
use rude_util::{apply_alias, format_lines_opt, relative_path};

pub(crate) struct TaggedEntry {
    pub idx: u32,
    pub tag: &'static str,
    pub sig: bool,
    pub call_line: u32,
}

pub(crate) fn load_or_build_graph() -> anyhow::Result<rude_intel::graph::CallGraph> {
    rude_intel::loader::load_or_build_graph()
}

pub(super) fn resolve_symbol(graph: &graph::CallGraph, symbol: &str) -> Option<Vec<u32>> {
    let seeds = graph.resolve(symbol);
    if seeds.is_empty() { println!("No symbol found matching \"{symbol}\"."); None } else { Some(seeds) }
}

#[inline]
pub(super) fn fmt_scope(scope: &Option<String>) -> String {
    scope.as_ref().map_or(String::new(), |s| format!(" (scope: {s})"))
}

pub(super) fn print_tagged(
    graph: &graph::CallGraph,
    entries: &[TaggedEntry],
    show_source: bool,
    alias_map: &BTreeMap<String, String>,
) {
    if entries.is_empty() { return; }

    let role_priority = |tag: &str| -> u8 {
        match tag { "def" | "target" | "field" => 0, "caller" | "d1" | "d2" | "d3+" => 1, "callee" => 2, "type" => 3, "test" => 4, _ => 5 }
    };
    let mut seen = std::collections::HashSet::<u32>::new();
    let mut sorted: Vec<&TaggedEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| role_priority(e.tag));
    let deduped: Vec<&TaggedEntry> = sorted.into_iter().filter(|e| seen.insert(e.idx)).collect();

    let files: Vec<&str> = deduped.iter().map(|e| relative_path(&graph.chunks[e.idx as usize].file)).collect();
    let mut groups: BTreeMap<&str, Vec<&TaggedEntry>> = BTreeMap::new();
    for (entry, file) in deduped.iter().zip(files.iter()) { groups.entry(file).or_default().push(entry); }

    for (file, items) in &groups {
        println!("@ {}", apply_alias(file, alias_map));
        for e in items {
            let i = e.idx as usize;
            let kind_tag = if graph.chunks[i].kind == "function" { String::new() } else { format!("[{}] ", graph.chunks[i].kind) };
            let test_marker = if graph.is_test[i] { " [test]" } else { "" };
            let call_site = if e.call_line > 0 { format!(" → :{}", e.call_line) } else { String::new() };
            println!("  [{}] {} {kind_tag}{}{test_marker}{call_site}", e.tag, format_lines_opt(graph.chunks[i].lines), graph.chunks[i].name);
            if e.sig { if let Some(s) = &graph.chunks[i].signature { println!("    {s}"); } }
            if show_source && (e.tag == "def" || e.sig) {
                if let Some((start, end)) = graph.chunks[i].lines {
                    if let Ok(content) = std::fs::read_to_string(&graph.chunks[i].file) {
                        let lines: Vec<&str> = content.lines().collect();
                        let s = start.saturating_sub(1);
                        let e2 = end.min(lines.len());
                        if s < e2 {
                            println!("    ```");
                            for (j, ln) in lines[s..e2].iter().enumerate() { println!("    {:>4}│ {ln}", s + j + 1); }
                            println!("    ```");
                        }
                    }
                }
            }
        }
        println!();
    }
}
