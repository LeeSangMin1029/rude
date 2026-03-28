use anyhow::Result;

use rude_intel::trace;
use rude_util::{apply_alias, format_lines_opt, relative_path};

use super::common::{load_or_build_graph, resolve_symbol};

pub fn run_trace(from: String, to: String) -> Result<()> {
    let graph = load_or_build_graph()?;
    let (alias_map, _) = graph.global_aliases();
    let Some(sources) = resolve_symbol(&graph, &from) else { return Ok(()) };
    let Some(targets) = resolve_symbol(&graph, &to) else { return Ok(()) };
    match trace::bfs_shortest_path(&graph, &sources, &targets) {
        Some(path) => {
            println!("=== trace: {from} \u{2192} {to} ({} hops) ===\n", path.len() - 1);
            for (step, &idx) in path.iter().enumerate() {
                let i = idx as usize;
                let short = apply_alias(relative_path(&graph.chunks[i].file), &alias_map);
                let test_marker = if graph.is_test[i] { " [test]" } else { "" };
                let (arrow, indent) = if step == 0 { ("  ", String::new()) } else { ("→ ", "  ".repeat(step)) };
                println!("  {indent}{arrow}{short}{}  {}{test_marker}", format_lines_opt(graph.chunks[i].lines), graph.chunks[i].name);
            }
            println!();
        }
        None => println!("No call path found from \"{from}\" to \"{to}\"."),
    }
    Ok(())
}
