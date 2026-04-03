use std::collections::BTreeMap;

use anyhow::Result;

use rude_intel::graph;
use rude_intel::parse::ParsedChunk;
use rude_util::{apply_alias, build_path_aliases, format_lines_opt, relative_path, shorten_signature};

use super::common::load_or_build_graph;

pub fn run_symbols(
    name: Option<String>,
    kind: Option<String>,
    include_tests: bool,
    limit: Option<usize>,
    compact: bool,
) -> Result<()> {
    let is_file_query = name.as_deref().is_some_and(looks_like_file_path);
    let graph = load_or_build_graph()?;
    let chunks = &graph.chunks;
    let all_files: Vec<&str> = chunks.iter().map(|c| relative_path(&c.file)).collect();
    let (alias_map, _) = build_path_aliases(&all_files);
    let filtered: Vec<&ParsedChunk> = chunks.iter().filter(|c| {
        if let Some(ref n) = name {
            if is_file_query {
                if !c.file.ends_with(n) && !c.file.ends_with(&n.replace('\\', "/")) { return false; }
            } else {
                let nl = n.to_lowercase();
                if !c.name.to_lowercase().contains(&nl) && !c.display_name.to_lowercase().contains(&nl) { return false; }
            }
        }
        if let Some(ref k) = kind && c.kind.to_lowercase() != k.to_lowercase() { return false; }
        if !include_tests && graph::is_test_chunk(c) { return false; }
        true
    }).collect();
    let total = filtered.len();
    let display: Vec<&ParsedChunk> = if let Some(n) = limit { filtered.into_iter().take(n).collect() } else { filtered };
    if display.is_empty() {
        println!("No symbols found.");
        return Ok(());
    }
    let suffix = limit.map_or(String::new(), |n| format!(" (showing {}/{})", display.len().min(n), total));
    let header = if is_file_query { format!("=== symbols: {total} in file ===\n") } else { format!("=== symbols: {total} found ===\n") };
    println!("{header}{suffix}");
    let files: Vec<&str> = display.iter().map(|c| relative_path(&c.file)).collect();
    let mut groups: BTreeMap<&str, Vec<&ParsedChunk>> = BTreeMap::new();
    for (c, file) in display.iter().zip(files.iter()) { groups.entry(file).or_default().push(c); }
    for (file, items) in &groups {
        println!("@ {}", apply_alias(file, &alias_map));
        for c in items {
            let kind_tag = if c.kind == "function" { String::new() } else { format!("[{}] ", c.kind) };
            let test_marker = if graph::is_test_chunk(c) { " [test]" } else { "" };
            println!("  {} {kind_tag}{}{test_marker}", format_lines_opt(c.lines), c.dn());
            if !compact { if let Some(sig) = c.signature.as_deref().filter(|s| !s.is_empty()) { println!("    {}", shorten_signature(sig, 120)); } }
        }
        if !compact { println!(); }
    }
    if !is_file_query {
        if let Some(ref n) = name {
            for idx in graph.resolve(n) {
                let i = idx as usize;
                let impls = &graph.trait_impls[i];
                if graph.chunks[i].kind != "trait" || impls.is_empty() { continue; }
                println!("  implementations of {}:", graph.chunks[i].dn());
                for ii in impls.iter().map(|&x| x as usize) {
                    println!("    {}{}  [{}] {}", relative_path(&graph.chunks[ii].file),
                        format_lines_opt(graph.chunks[ii].lines), graph.chunks[ii].kind, graph.chunks[ii].dn());
                }
                println!();
            }
        }
    }
    Ok(())
}

fn looks_like_file_path(s: &str) -> bool {
    const EXTENSIONS: &[&str] = &[
        ".rs", ".go", ".py", ".js", ".ts", ".tsx", ".jsx",
        ".c", ".cpp", ".cc", ".h", ".hpp", ".java", ".kt", ".cs", ".rb", ".swift",
    ];
    EXTENSIONS.iter().any(|ext| s.ends_with(ext)) || s.contains('/')
}
