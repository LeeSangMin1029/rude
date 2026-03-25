//! `rude cluster` — find independent function clusters within a file.

use std::path::PathBuf;

use anyhow::Result;

use super::intel::load_or_build_graph;

/// Run the cluster analysis command.
pub fn run(db: PathBuf, file: String, min_lines: usize) -> Result<()> {
    let graph = load_or_build_graph(&db)?;
    let n = graph.names.len();

    // 1. Collect function indices belonging to the target file.
    let mut file_indices: Vec<usize> = Vec::new();
    for i in 0..n {
        if graph.kinds[i] != "function" {
            continue;
        }
        if graph.files[i].contains(file.as_str()) || graph.files[i].ends_with(&file) {
            file_indices.push(i);
        }
    }

    if file_indices.is_empty() {
        println!("No functions found matching file filter: {file}");
        return Ok(());
    }

    // 2. Union-Find over file_indices.
    let idx_count = file_indices.len();
    let mut parent: Vec<usize> = (0..idx_count).collect();
    let mut rank: Vec<usize> = vec![0; idx_count];

    // Map global index → local index for union-find.
    let global_to_local: std::collections::HashMap<usize, usize> = file_indices
        .iter()
        .enumerate()
        .map(|(local, &global)| (global, local))
        .collect();

    fn find(parent: &mut [usize], x: usize) -> usize {
        if parent[x] != x {
            parent[x] = find(parent, parent[x]);
        }
        parent[x]
    }

    fn union(parent: &mut [usize], rank: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra == rb {
            return;
        }
        if rank[ra] < rank[rb] {
            parent[ra] = rb;
        } else if rank[ra] > rank[rb] {
            parent[rb] = ra;
        } else {
            parent[rb] = ra;
            rank[ra] += 1;
        }
    }

    // 4. For each function in this file, check callees/callers that are also in this file.
    for &gi in &file_indices {
        let li = global_to_local[&gi];
        for &callee in &graph.callees[gi] {
            let callee = callee as usize;
            if let Some(&lj) = global_to_local.get(&callee) {
                union(&mut parent, &mut rank, li, lj);
            }
        }
        for &caller in &graph.callers[gi] {
            let caller = caller as usize;
            if let Some(&lj) = global_to_local.get(&caller) {
                union(&mut parent, &mut rank, li, lj);
            }
        }
    }

    // 5. Group by component.
    let mut components: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for li in 0..idx_count {
        let root = find(&mut parent, li);
        components.entry(root).or_default().push(li);
    }

    // Sort components by total lines descending.
    let mut groups: Vec<Vec<usize>> = components.into_values().collect();

    // Compute lines per group for sorting.
    let line_count = |members: &[usize]| -> usize {
        members
            .iter()
            .map(|&li| {
                let gi = file_indices[li];
                graph.lines[gi]
                    .map(|(s, e)| if e >= s { e - s + 1 } else { 0 })
                    .unwrap_or(0)
            })
            .sum()
    };

    groups.sort_by(|a, b| line_count(b).cmp(&line_count(a)));

    // Sort members within each group by start line.
    for g in &mut groups {
        g.sort_by_key(|&li| {
            let gi = file_indices[li];
            graph.lines[gi].map(|(s, _)| s).unwrap_or(0)
        });
    }

    // 6. Print results.
    println!(
        "\n=== clusters in {file}: {} groups ===\n",
        groups.len()
    );

    for (idx, members) in groups.iter().enumerate() {
        let total_lines = line_count(members);
        let fn_count = members.len();
        let is_candidate = total_lines >= min_lines;
        let tag = if is_candidate { " [split candidate]" } else { "" };

        println!(
            "Group {} ({} lines, {} functions){tag}:",
            idx + 1,
            total_lines,
            fn_count,
        );

        for &li in members {
            let gi = file_indices[li];
            let line_range = graph.lines[gi]
                .map(|(s, e)| format!(":{s}-{e}"))
                .unwrap_or_default();
            println!("  {line_range:<12} {}", graph.names[gi]);
        }
        println!();
    }

    Ok(())
}
