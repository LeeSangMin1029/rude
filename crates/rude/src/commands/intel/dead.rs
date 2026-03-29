
use anyhow::Result;

use rude_util::{apply_alias, extract_crate_name, format_lines_opt, relative_path};

use super::query::load_or_build_graph;

pub fn run_dead(
    include_pub: bool,
    file_filter: Option<String>,
) -> Result<()> {

    let graph = load_or_build_graph()?;
    let n = graph.chunks.len();
    let (alias_map, _) = graph.global_aliases();

    let mut dead: Vec<usize> = Vec::new();

    for i in 0..n {
        if graph.is_test[i] || graph.chunks[i].kind != "function" {
            continue;
        }
        if is_derive_generated(&graph.chunks[i].name) {
            continue;
        }
        if let Some(ref filter) = file_filter {
            if !graph.chunks[i].file.contains(filter.as_str()) {
                continue;
            }
        }
        if !graph.callers[i].is_empty() {
            continue;
        }
        if !include_pub {
            let is_pub = graph.chunks[i].signature
                .as_deref()
                .is_some_and(|s| s.starts_with("pub ") || s.starts_with("pub(crate)"));
            if is_pub {
                continue;
            }
        }
        if graph.chunks[i].name.starts_with('<') && graph.chunks[i].name.contains(" as ") {
            continue;
        }
        let name = &graph.chunks[i].name;
        if name == "main" || name.ends_with("::main") {
            continue;
        }
        let file = &graph.chunks[i].file;
        if !file.ends_with(".rs") {
            continue;
        }
        if name.contains("::check::assert_impl")
            || name.contains("::{closure#0}::check")
            || name.ends_with("::new") && graph.callees[i].is_empty()
        {
            continue;
        }

        dead.push(i);
    }

    let mut by_crate: std::collections::BTreeMap<String, Vec<usize>> = std::collections::BTreeMap::new();
    for &i in &dead {
        let c = &graph.chunks[i];
        let crate_name = if c.crate_name.is_empty() { extract_crate_name(&c.file) } else { c.crate_name.clone() };
        by_crate.entry(crate_name).or_default().push(i);
    }

    println!("=== dead code: {} functions with no callers ===\n", dead.len());

    for (crate_name, indices) in &by_crate {
        println!("[{}] {} dead:", crate_name, indices.len());
        for &i in indices {
            let loc = format_lines_opt(graph.chunks[i].lines);
            let rel = relative_path(&graph.chunks[i].file);
            let short = apply_alias(rel, &alias_map);
            println!("  {short}{loc}  {}", graph.chunks[i].name);
        }
        println!();
    }

    Ok(())
}

fn is_derive_generated(name: &str) -> bool {
    name.contains("::_serde::") || name.contains("::_::_serde::")
    || name.contains("as bincode::Encode>::encode")
    || name.contains("as bincode::Decode<")
    || name.contains("as bincode::BorrowDecode<")
    || name.contains("as clap::")
}
