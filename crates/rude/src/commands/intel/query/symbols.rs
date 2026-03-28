use std::collections::BTreeMap;

use anyhow::Result;

use rude_intel::graph;
use rude_intel::parse::ParsedChunk;
use rude_util::{apply_alias, build_path_aliases, format_lines_opt, relative_path};

use super::common::load_or_build_graph;

pub fn run_symbols(
    name: Option<String>,
    kind: Option<String>,
    include_tests: bool,
    limit: Option<usize>,
    compact: bool,
) -> Result<()> {
    let is_file_query = name.as_deref().is_some_and(looks_like_file_path);
    run_chunk_query(
        |c| {
            if let Some(ref n) = name {
                if is_file_query {
                    if !c.file.ends_with(n) && !c.file.ends_with(&n.replace('\\', "/")) { return false; }
                } else {
                    if !c.name.to_lowercase().contains(&n.to_lowercase()) { return false; }
                }
            }
            if let Some(ref k) = kind && c.kind.to_lowercase() != k.to_lowercase() { return false; }
            if !include_tests && rude_intel::graph::is_test_chunk(c) { return false; }
            true
        },
        "No symbols found.",
        |n| if is_file_query { format!("=== symbols: {n} in file ===\n") } else { format!("=== symbols: {n} found ===\n") },
        limit,
        compact,
    )?;
    if !is_file_query { print_trait_impls_if_relevant(name.as_deref())?; }
    Ok(())
}

fn looks_like_file_path(s: &str) -> bool {
    const EXTENSIONS: &[&str] = &[
        ".rs", ".go", ".py", ".js", ".ts", ".tsx", ".jsx",
        ".c", ".cpp", ".cc", ".h", ".hpp", ".java", ".kt", ".cs", ".rb", ".swift",
    ];
    EXTENSIONS.iter().any(|ext| s.ends_with(ext)) || s.contains('/')
}

fn print_trait_impls_if_relevant(name: Option<&str>) -> Result<()> {
    let Some(name) = name else { return Ok(()) };
    let Some(graph) = graph::CallGraph::load(crate::db()) else { return Ok(()) };
    for idx in graph.resolve(name) {
        let i = idx as usize;
        let impls = &graph.trait_impls[i];
        if graph.chunks[i].kind != "trait" || impls.is_empty() { continue; }
        println!("  implementations of {}:", graph.chunks[i].name);
        for ii in impls.iter().map(|&x| x as usize) {
            println!("    {}{}  [{}] {}", relative_path(&graph.chunks[ii].file),
                format_lines_opt(graph.chunks[ii].lines), graph.chunks[ii].kind, graph.chunks[ii].name);
        }
        println!();
    }
    Ok(())
}

fn run_chunk_query(
    filter: impl Fn(&ParsedChunk) -> bool,
    empty_msg: &str,
    header: impl FnOnce(usize) -> String,
    limit: Option<usize>,
    compact: bool,
) -> Result<()> {
    let graph = load_or_build_graph()?;
    let chunks = &graph.chunks;
    let all_files: Vec<&str> = chunks.iter().map(|c| relative_path(&c.file)).collect();
    let (alias_map, _) = build_path_aliases(&all_files);

    let filtered: Vec<&ParsedChunk> = chunks.iter().filter(|c| filter(c)).collect();
    let total = filtered.len();
    let display: Vec<&ParsedChunk> = if let Some(n) = limit { filtered.into_iter().take(n).collect() } else { filtered };

    if display.is_empty() {
        println!("{empty_msg}");
        return Ok(());
    }

    let suffix = limit.map_or(String::new(), |n| format!(" (showing {}/{})", display.len().min(n), total));
    println!("{}{suffix}", header(total));

    let files: Vec<&str> = display.iter().map(|c| relative_path(&c.file)).collect();
    let mut groups: BTreeMap<&str, Vec<&ParsedChunk>> = BTreeMap::new();
    for (c, file) in display.iter().zip(files.iter()) { groups.entry(file).or_default().push(c); }
    for (file, items) in &groups {
        println!("@ {}", apply_alias(file, &alias_map));
        for c in items {
            let kind_tag = if c.kind == "function" { String::new() } else { format!("[{}] ", c.kind) };
            let test_marker = if graph::is_test_chunk(c) { " [test]" } else { "" };
            println!("  {} {kind_tag}{}{test_marker}", format_lines_opt(c.lines), c.name);
            if !compact { if let Some(sig) = c.signature.as_deref().filter(|s| !s.is_empty()) { println!("    {sig}"); } }
        }
        if !compact { println!(); }
    }
    Ok(())
}
