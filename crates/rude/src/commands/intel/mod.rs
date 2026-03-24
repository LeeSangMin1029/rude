//! Code intelligence commands — structural queries on code-chunked databases.
//!
//! Provides `symbols`, `context`, and `trace`
//! subcommands that parse the structured text field of code chunks
//! (produced by `chunk_code`) and answer structural navigation queries.
//!
//! These commands are read-only and do not modify the database.

mod commands;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

// ── Re-exports: CLI command handlers ─────────────────────────────────────

pub use commands::{run_aliases, run_stats, run_symbols, run_context, run_trace, run_coverage, run_dead};

// ── Re-exports: library types for submodules and external consumers ──────

#[cfg(test)]
pub use rude_intel::context;
pub use rude_intel::graph;
pub use rude_intel::helpers::{format_lines_opt, format_lines_str_opt, relative_path};
pub use rude_intel::impact;
pub use rude_intel::loader::load_chunks;
/// Load or build call graph (MIR edges → name-resolve fallback).
pub fn load_or_build_graph(
    db: &std::path::Path,
) -> anyhow::Result<rude_intel::graph::CallGraph> {
    rude_intel::loader::load_or_build_graph(db)
}

/// Load or build call graph, also returning chunks if they were loaded.
pub fn load_or_build_graph_with_chunks(
    db: &std::path::Path,
) -> anyhow::Result<(rude_intel::graph::CallGraph, Option<Vec<rude_intel::parse::ParsedChunk>>)> {
    rude_intel::loader::load_or_build_graph_with_chunks(db)
}

pub use rude_intel::parse::ParsedChunk;
#[cfg(test)]
pub use rude_intel::parse;
pub use rude_intel::stats::build_stats;
pub use rude_intel::trace;

// ── Shared utilities (used by commands.rs) ────────────────────────────────

/// Print chunks grouped by file with path aliases.
pub(crate) fn print_grouped(
    chunks: &[&ParsedChunk],
    compact: bool,
    alias_map: &std::collections::BTreeMap<String, String>,
) {
    use rude_intel::helpers::apply_alias;

    let files: Vec<&str> = chunks.iter().map(|c| relative_path(&c.file)).collect();

    let mut groups: BTreeMap<&str, Vec<&ParsedChunk>> = BTreeMap::new();
    for (c, file) in chunks.iter().zip(files.iter()) {
        groups.entry(file).or_default().push(c);
    }

    for (file, items) in &groups {
        let short = apply_alias(file, alias_map);
        println!("@ {short}");
        for c in items {
            let lines = format_lines_opt(c.lines);
            let test_marker = if rude_intel::graph::is_test_chunk(c) { " [test]" } else { "" };
            let kind_tag = if c.kind == "function" { String::new() } else { format!("[{}] ", c.kind) };
            println!("  {lines} {kind_tag}{name}{test_marker}", name = c.name);
            if !compact {
                let sig = c.signature.as_deref().unwrap_or("");
                if !sig.is_empty() {
                    println!("    {sig}");
                }
            }
        }
        if !compact { println!(); }
    }
}
