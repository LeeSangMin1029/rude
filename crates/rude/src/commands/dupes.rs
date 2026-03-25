// serde_json::to_string only fails on non-string map keys, which we don't use.
#![expect(clippy::expect_used)]

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Serialize;
use rude_db::PayloadStore;
use rude_intel::clones::{
    self, DupePair, RunStages, SubBlockClone, UnifiedDupePair,
};
use rude_db::StorageEngine;

pub struct DupesConfig {
    pub threshold: f32,
    pub exclude_tests: bool,
    pub k: usize,
    pub json: bool,
    pub ast_mode: bool,
    pub all_mode: bool,
    pub min_lines: usize,
    pub min_sub_lines: usize,
    pub analyze: bool,
}

pub fn run(cfg: DupesConfig) -> Result<()> {
    let DupesConfig {
        threshold, exclude_tests, k, json,
        ast_mode, all_mode, min_lines, min_sub_lines, analyze,
    } = cfg;
    let db = crate::db();
    let engine = StorageEngine::open(db)
        .with_context(|| format!("failed to open database at {}", db.display()))?;
    let pstore = engine.payload_store();
    let candidate_ids = clones::collect_filtered_ids(&engine, pstore, exclude_tests, min_lines);

    let n = candidate_ids.len();
    if n < 2 {
        println!("Not enough chunks for comparison ({n}).");
        return Ok(());
    }

    let mut pair_names: Vec<(String, String)> = Vec::new();

    if ast_mode {
        let groups = clones::find_hash_groups(pstore, &candidate_ids, "ast_hash", k);
        if json {
            print_groups_json(&groups, pstore);
        } else {
            print_groups_text(&groups, pstore);
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
        let pairs = clones::find_minhash_pairs(pstore, &candidate_ids, threshold, k);

        if json {
            print_pairs_json(&pairs, pstore);
        } else {
            print_pairs_text(&pairs, pstore);
        }

        if analyze {
            for p in &pairs {
                let a = parse_label(pstore, p.id_a).name;
                let b = parse_label(pstore, p.id_b).name;
                pair_names.push((a, b));
            }
        }
    } else {
        eprintln!("Unified pipeline: {n} chunks");

        let (unified_pairs, sub_clones) =
            clones::run_unified_pipeline(&engine, pstore, &candidate_ids, threshold, k, &stages, min_sub_lines)?;

        if unified_pairs.is_empty() {
            println!("No duplicates found.");
        } else if json {
            print_pairs_json(&unified_pairs, pstore);
        } else {
            print_pairs_text(&unified_pairs, pstore);
        }

        if !sub_clones.is_empty() {
            let capped: Vec<_> = sub_clones.into_iter().take(k).collect();
            if json {
                print_sub_block_json(&capped, pstore);
            } else {
                print_sub_block_text(&capped, pstore);
            }
        }

        if analyze {
            for p in &unified_pairs {
                let a = parse_label(pstore, p.id_a).name;
                let b = parse_label(pstore, p.id_b).name;
                pair_names.push((a, b));
            }
        }
    }

    if analyze && !pair_names.is_empty() {
        run_analyze(&pair_names)?;
    }

    Ok(())
}

trait DupePairLike {
    fn id_a(&self) -> u64;
    fn id_b(&self) -> u64;
    fn display_score(&self) -> f32;
    fn display_tag(&self) -> String { String::new() }
    fn to_json_value(&self, a: String, b: String) -> serde_json::Value;
}

impl DupePairLike for DupePair {
    fn id_a(&self) -> u64 { self.id_a }
    fn id_b(&self) -> u64 { self.id_b }
    fn display_score(&self) -> f32 { self.similarity }
    fn to_json_value(&self, a: String, b: String) -> serde_json::Value {
        serde_json::json!({ "a": a, "b": b, "sim": self.similarity })
    }
}

impl DupePairLike for UnifiedDupePair {
    fn id_a(&self) -> u64 { self.id_a }
    fn id_b(&self) -> u64 { self.id_b }
    fn display_score(&self) -> f32 { self.score }
    fn display_tag(&self) -> String { self.tag() }
    fn to_json_value(&self, a: String, b: String) -> serde_json::Value {
        serde_json::json!({ "a": a, "b": b, "score": self.score, "jaccard": self.jaccard, "ast_match": self.ast_match, "tag": self.tag() })
    }
}

struct GroupEntry {
    pair_index: usize,
    name_a: String,
    name_b: String,
}

struct ChunkLabel {
    name: String,
    file: String,
}

impl ChunkLabel {
    fn display(&self) -> String {
        if self.file.is_empty() {
            self.name.clone()
        } else {
            format!("{}  ({})", self.name, self.file)
        }
    }
}

fn parse_label(pstore: &(impl PayloadStore + ?Sized), id: u64) -> ChunkLabel {
    let Some(text) = pstore.get_text(id).ok().flatten() else {
        return ChunkLabel {
            name: format!("id:{id}"),
            file: String::new(),
        };
    };
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let file = lines
        .next()
        .unwrap_or("")
        .strip_prefix("File: ")
        .unwrap_or("")
        .to_owned();
    let name = first
        .strip_prefix("[function] ")
        .or_else(|| first.strip_prefix("[impl] "))
        .or_else(|| first.strip_prefix("[struct] "))
        .unwrap_or(first)
        .to_owned();
    ChunkLabel { name, file }
}

fn label(pstore: &(impl PayloadStore + ?Sized), id: u64) -> String {
    parse_label(pstore, id).display()
}

use rude_intel::helpers::{apply_alias, build_path_aliases};
use rude_intel::parse::normalize_path;

fn group_by_file(
    ids: &[(u64, u64)],
    pstore: &(impl PayloadStore + ?Sized),
) -> Vec<(String, Vec<GroupEntry>)> {
    let labels: Vec<(ChunkLabel, ChunkLabel)> = ids
        .iter()
        .map(|&(a, b)| (parse_label(pstore, a), parse_label(pstore, b)))
        .collect();

    let all_files: Vec<String> = labels
        .iter()
        .flat_map(|(a, b)| [normalize_path(&a.file), normalize_path(&b.file)])
        .collect();

    let (alias_map, _) = build_path_aliases(&all_files.iter().map(String::as_str).collect::<Vec<_>>());

    let mut by_file: Vec<(String, Vec<GroupEntry>)> = Vec::new();
    let mut file_index: HashMap<String, usize> = HashMap::new();

    for (i, _) in ids.iter().enumerate() {
        let raw = &all_files[i * 2];
        let key = if raw.is_empty() { "(unknown)".to_owned() } else { apply_alias(raw, &alias_map) };
        let idx = *file_index.entry(key.clone()).or_insert_with(|| {
            let i = by_file.len();
            by_file.push((key, Vec::new()));
            i
        });
        by_file[idx].1.push(GroupEntry {
            pair_index: i,
            name_a: labels[i].0.name.clone(),
            name_b: labels[i].1.name.clone(),
        });
    }

    by_file.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    by_file
}

fn print_json<T: Serialize>(val: &T) {
    println!("{}", serde_json::to_string(val).expect("JSON serialize"));
}

fn print_pairs_text(
    pairs: &[impl DupePairLike],
    pstore: &(impl PayloadStore + ?Sized),
) {
    if pairs.is_empty() {
        println!("No duplicates found above threshold.");
        return;
    }
    println!("{} duplicate pairs found:\n", pairs.len());

    let ids: Vec<(u64, u64)> = pairs.iter().map(|p| (p.id_a(), p.id_b())).collect();
    let groups = group_by_file(&ids, pstore);

    for (file, entries) in &groups {
        println!("  {file} ({} pairs)", entries.len());
        for e in entries {
            let p = &pairs[e.pair_index];
            let score = p.display_score();
            let tag = p.display_tag();
            let tag_part = if tag.is_empty() { " ".to_owned() } else { format!(" {tag:<12} ") };
            println!("    [{score:.2}]{tag_part}{} \u{2194} {}", e.name_a, e.name_b);
        }
        println!();
    }

    eprintln!("Tip: rude context <symbol>  to inspect.");
}

fn print_pairs_json(pairs: &[impl DupePairLike], pstore: &(impl PayloadStore + ?Sized)) {
    let vals: Vec<_> = pairs.iter()
        .map(|p| p.to_json_value(label(pstore, p.id_a()), label(pstore, p.id_b())))
        .collect();
    print_json(&serde_json::json!({ "pairs": vals }));
}

fn print_groups_text(groups: &[(u64, Vec<u64>)], pstore: &(impl PayloadStore + ?Sized)) {
    if groups.is_empty() {
        println!("No clones found.");
        return;
    }
    println!("{} clone groups found:\n", groups.len());

    // Collect (group_num, hash, label) entries and all file paths for alias building.
    let entries_flat: Vec<(usize, u64, ChunkLabel)> = groups
        .iter()
        .enumerate()
        .flat_map(|(gi, (hash, ids))| {
            ids.iter().map(move |&id| (gi + 1, *hash, parse_label(pstore, id)))
        })
        .collect();

    let all_files: Vec<String> = entries_flat.iter().map(|(_, _, cl)| normalize_path(&cl.file)).collect();
    let (alias_map, _) = build_path_aliases(&all_files.iter().map(String::as_str).collect::<Vec<_>>());

    type FileGroup = (String, Vec<(usize, u64, String)>);
    let mut by_file: Vec<FileGroup> = Vec::new();
    let mut file_index: HashMap<String, usize> = HashMap::new();

    for (i, (group_num, hash, cl)) in entries_flat.iter().enumerate() {
        let key = if all_files[i].is_empty() { "(unknown)".to_owned() } else { apply_alias(&all_files[i], &alias_map) };
        let idx = *file_index.entry(key.clone()).or_insert_with(|| {
            let n = by_file.len();
            by_file.push((key, Vec::new()));
            n
        });
        by_file[idx].1.push((*group_num, *hash, cl.name.clone()));
    }

    by_file.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    for (file, entries) in &by_file {
        println!("  {file} ({} clones)", entries.len());
        for (group_num, hash, name) in entries {
            println!("    G{group_num} [{hash:016x}]  {name}");
        }
        println!();
    }

    eprintln!("Tip: rude context <symbol>  to inspect.");
}

fn print_groups_json(groups: &[(u64, Vec<u64>)], pstore: &(impl PayloadStore + ?Sized)) {
    #[derive(Serialize)]
    struct GroupJson { hash: String, members: Vec<String> }
    #[derive(Serialize)]
    struct Out { groups: Vec<GroupJson> }
    print_json(&Out {
        groups: groups.iter().map(|(hash, ids)| GroupJson {
            hash: format!("{hash:016x}"),
            members: ids.iter().map(|&id| label(pstore, id)).collect(),
        }).collect(),
    });
}

fn print_sub_block_text(clones: &[SubBlockClone], pstore: &(impl PayloadStore + ?Sized)) {
    println!("\n{} sub-block clones (intra-function):\n", clones.len());

    let labels: Vec<(ChunkLabel, ChunkLabel)> = clones
        .iter()
        .map(|c| (parse_label(pstore, c.chunk_id_a), parse_label(pstore, c.chunk_id_b)))
        .collect();
    let all_files: Vec<String> = labels
        .iter()
        .flat_map(|(a, b)| [normalize_path(&a.file), normalize_path(&b.file)])
        .collect();
    let (alias_map, _) = build_path_aliases(&all_files.iter().map(String::as_str).collect::<Vec<_>>());

    for (i, c) in clones.iter().enumerate() {
        let (name_a, name_b) = (&labels[i].0.name, &labels[i].1.name);
        let file_a = apply_alias(&all_files[i * 2], &alias_map);
        let file_b = apply_alias(&all_files[i * 2 + 1], &alias_map);
        let body_tag = if c.body_match { " [exact]" } else { "" };
        println!(
            "    {name_a}  ({file_a} L{}-{}) \u{2194} {name_b}  ({file_b} L{}-{}){body_tag}",
            c.block_a_start + 1, c.block_a_end + 1, c.block_b_start + 1, c.block_b_end + 1,
        );
    }
    println!();
}

fn print_sub_block_json(clones: &[SubBlockClone], pstore: &(impl PayloadStore + ?Sized)) {
    #[derive(Serialize)]
    struct SubBlockJson { a: String, a_lines: [usize; 2], b: String, b_lines: [usize; 2], body_match: bool }
    #[derive(Serialize)]
    struct Out { sub_block_clones: Vec<SubBlockJson> }
    print_json(&Out {
        sub_block_clones: clones.iter().map(|c| SubBlockJson {
            a: label(pstore, c.chunk_id_a),
            a_lines: [c.block_a_start + 1, c.block_a_end + 1],
            b: label(pstore, c.chunk_id_b),
            b_lines: [c.block_b_start + 1, c.block_b_end + 1],
            body_match: c.body_match,
        }).collect(),
    });
}

fn run_analyze(pair_names: &[(String, String)]) -> Result<()> {
    use rude_intel::dupe_analyze;
    use super::intel::load_or_build_graph;

    let graph = load_or_build_graph()?;

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
    let analyses = dupe_analyze::analyze_pairs(&graph, &idx_pairs);

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
