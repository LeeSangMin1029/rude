use anyhow::Result;
use rude_intel::clones::{self, RunStages};

use super::DupesConfig;
use super::types::parse_label;
use super::output::{
    print_pairs_text, print_pairs_json,
    print_groups_text, print_groups_json,
    print_sub_block_text, print_sub_block_json,
};
use super::analyze::run_analyze;

pub fn run(cfg: DupesConfig) -> Result<()> {
    let DupesConfig {
        threshold, exclude_tests, k, json,
        ast_mode, all_mode, min_lines, min_sub_lines, analyze,
    } = cfg;

    use crate::commands::intel::load_or_build_graph;
    let graph = load_or_build_graph()?;
    let chunks = &graph.chunks;

    let candidate_indices = clones::collect_filtered_indices(chunks, exclude_tests, min_lines);

    let n = candidate_indices.len();
    if n < 2 {
        println!("Not enough chunks for comparison ({n}).");
        return Ok(());
    }

    let mut pair_names: Vec<(String, String)> = Vec::new();

    if ast_mode {
        let groups = clones::find_hash_groups(chunks, &candidate_indices, k);
        if json {
            print_groups_json(&groups, chunks);
        } else {
            print_groups_text(&groups, chunks);
        }
        return Ok(());
    }

    let stages = if all_mode {
        RunStages { ast: true, minhash: true }
    } else {
        RunStages { ast: false, minhash: true }
    };

    let is_unified = stages.ast && stages.minhash;

    if !is_unified {
        eprintln!("Comparing {n} chunks (Jaccard threshold={threshold:.2})...");
        let pairs = clones::find_minhash_pairs(chunks, &candidate_indices, threshold, k);

        if json {
            print_pairs_json(&pairs, chunks);
        } else {
            print_pairs_text(&pairs, chunks);
        }

        if analyze {
            for p in &pairs {
                let a = parse_label(chunks, p.idx_a).name;
                let b = parse_label(chunks, p.idx_b).name;
                pair_names.push((a, b));
            }
        }
    } else {
        eprintln!("Unified pipeline: {n} chunks");

        let (unified_pairs, sub_clones) =
            clones::run_unified_pipeline(chunks, &candidate_indices, threshold, k, &stages, min_sub_lines)?;

        if unified_pairs.is_empty() {
            println!("No duplicates found.");
        } else if json {
            print_pairs_json(&unified_pairs, chunks);
        } else {
            print_pairs_text(&unified_pairs, chunks);
        }

        if !sub_clones.is_empty() {
            let capped: Vec<_> = sub_clones.into_iter().take(k).collect();
            if json {
                print_sub_block_json(&capped, chunks);
            } else {
                print_sub_block_text(&capped, chunks);
            }
        }

        if analyze {
            for p in &unified_pairs {
                let a = parse_label(chunks, p.idx_a).name;
                let b = parse_label(chunks, p.idx_b).name;
                pair_names.push((a, b));
            }
        }
    }

    if analyze && !pair_names.is_empty() {
        run_analyze(&pair_names, &graph)?;
    }

    Ok(())
}
