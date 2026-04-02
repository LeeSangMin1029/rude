use anyhow::Result;

use rude_intel::stats::build_stats;

use super::common::load_or_build_graph;

pub fn run_aliases() -> Result<()> {
    let graph = load_or_build_graph()?;
    let (_alias_map, legend) = graph.global_aliases();
    for (alias, dir) in &legend { println!("{alias} = {dir}"); }
    Ok(())
}

pub fn run_stats() -> Result<()> {
    let graph = load_or_build_graph()?;
    let chunks = &graph.chunks;
    let stats = build_stats(&chunks);
    println!("=== stats: {} crates ===\n", stats.len());
    println!("{:<28} {:>8} {:>8} {:>8} {:>8}", "crate", "prod_fn", "test_fn", "struct", "enum");
    println!("{}", "-".repeat(72));
    let mut totals = [0usize; 4];
    for (name, row) in &stats {
        println!("{:<28} {:>8} {:>8} {:>8} {:>8}", name, row[0], row[1], row[2], row[3]);
        for (i, v) in row.iter().enumerate() { totals[i] += v; }
    }
    println!("{}", "-".repeat(72));
    println!("{:<28} {:>8} {:>8} {:>8} {:>8}", "total", totals[0], totals[1], totals[2], totals[3]);
    Ok(())
}
