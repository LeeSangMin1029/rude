use anyhow::Result;

pub(crate) fn run_analyze(pair_names: &[(String, String)], graph: &rude_intel::graph::CallGraph) -> Result<()> {
    use rude_intel::dupe_analyze;

    let mut resolved_pairs: Vec<(u32, u32, &str, &str)> = Vec::new();
    for (name_a, name_b) in pair_names {
        let indices_a = graph.resolve(name_a);
        let indices_b = graph.resolve(name_b);
        if let (Some(&idx_a), Some(&idx_b)) = (indices_a.first(), indices_b.first()) {
            resolved_pairs.push((idx_a, idx_b, name_a.as_str(), name_b.as_str()));
        }
    }

    if resolved_pairs.is_empty() {
        return Ok(());
    }

    let idx_pairs: Vec<(u32, u32)> = resolved_pairs.iter().map(|&(a, b, _, _)| (a, b)).collect();
    let analyses = dupe_analyze::analyze_pairs(graph, &idx_pairs);

    println!("\n=== Analysis ===\n");

    for (i, analysis) in analyses.iter().enumerate() {
        let (_, _, name_a, name_b) = &resolved_pairs[i];
        let callee_pct = (analysis.callee_match_pct * 100.0) as u32;
        let caller_pct = (analysis.caller_match_pct * 100.0) as u32;
        println!(
            "[{}] {} \u{2194} {}",
            i + 1,
            name_a,
            name_b,
        );
        println!(
            "    callee: {callee_pct}%  caller: {caller_pct}%  blast: {} affected ({} prod, {} test)",
            analysis.blast_total, analysis.blast_prod, analysis.blast_test,
        );
        println!("    \u{2192} {}\n", analysis.verdict.label());
    }

    Ok(())
}
