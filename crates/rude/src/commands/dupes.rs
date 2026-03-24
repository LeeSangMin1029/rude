//! `dupes` command — find duplicate code chunks.
//!
//! Three modes:
//! - **Token Jaccard** (default): MinHash fingerprint-based near-duplicate detection.
//!   Compares actual code body tokens (unigrams + bigrams). Catches Type-1~3 clones.
//! - **AST hash** (`--ast`): structural clones ignoring identifier names (Type-1/2).
//! - **All** (`--all`): unified Filter→Verify pipeline combining AST + MinHash signals.
//!
//! Detection algorithms live in [`rude_intel::clones`]; this module provides
//! CLI argument handling and output formatting only.

// serde_json::to_string only fails on non-string map keys, which we don't use.
#![expect(clippy::expect_used)]

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use rude_db::PayloadStore;
use rude_intel::clones::{
    self, DupePair, RunStages, SubBlockClone, UnifiedDupePair,
};
use rude_db::StorageEngine;

// ── JSON output types ────────────────────────────────────────────────────

#[derive(Serialize)]
struct PairsOutput {
    pairs: Vec<PairJson>,
}

#[derive(Serialize)]
struct PairJson {
    a: String,
    b: String,
    sim: f32,
}

#[derive(Serialize)]
struct UnifiedPairJson {
    a: String,
    b: String,
    score: f32,
    jaccard: f32,
    ast_match: bool,
    tag: String,
}

#[derive(Serialize)]
struct UnifiedPairsOutput {
    pairs: Vec<UnifiedPairJson>,
}

#[derive(Serialize)]
struct GroupJson {
    hash: String,
    members: Vec<String>,
}

#[derive(Serialize)]
struct GroupsOutput {
    groups: Vec<GroupJson>,
}

#[derive(Serialize)]
struct SubBlockJson {
    a: String,
    a_lines: [usize; 2],
    b: String,
    b_lines: [usize; 2],
    body_match: bool,
}

#[derive(Serialize)]
struct SubBlocksOutput {
    sub_block_clones: Vec<SubBlockJson>,
}

// ── CLI entry point ──────────────────────────────────────────────────────

/// Configuration for the dupes command.
pub struct DupesConfig {
    pub db: std::path::PathBuf,
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

/// Run the dupes command.
pub fn run(cfg: DupesConfig) -> Result<()> {
    let DupesConfig {
        db, threshold, exclude_tests, k, json,
        ast_mode, all_mode, min_lines, min_sub_lines, analyze,
    } = cfg;
    let engine = StorageEngine::open(&db)
        .with_context(|| format!("failed to open database at {}", db.display()))?;
    let pstore = engine.payload_store();
    let candidate_ids = clones::collect_filtered_ids(&engine, pstore, exclude_tests, min_lines);

    let n = candidate_ids.len();
    if n < 2 {
        println!("Not enough chunks for comparison ({n}).");
        return Ok(());
    }

    // Collect pair names for --analyze post-processing.
    let mut pair_names: Vec<(String, String)> = Vec::new();

    if ast_mode {
        let groups = clones::find_hash_groups(pstore, &candidate_ids, "ast_hash", k);
        if json {
            print_groups_json(&groups, pstore);
        } else {
            print_groups_text(&groups, pstore, &db);
        }
        // AST mode produces groups, not pairs — skip analyze.
        return Ok(());
    }

    let stages = if all_mode {
        RunStages { ast: true, minhash: true }
    } else {
        RunStages { ast: false, minhash: true }
    };

    let is_unified = stages.ast && stages.minhash;

    // Single-signal fast path (MinHash only)
    if !is_unified {
        eprintln!("Comparing {n} chunks (Jaccard threshold={threshold:.2})...");
        let pairs = clones::find_minhash_pairs(pstore, &candidate_ids, threshold, k);

        if json {
            print_pairs_json(&pairs, pstore);
        } else {
            print_pairs_text(&pairs, pstore, &db);
        }

        if analyze {
            for p in &pairs {
                let a = parse_label(pstore, p.id_a).name;
                let b = parse_label(pstore, p.id_b).name;
                pair_names.push((a, b));
            }
        }
    } else {
        // Unified multi-signal pipeline
        eprintln!("Unified pipeline: {n} chunks");

        let (unified_pairs, sub_clones) =
            clones::run_unified_pipeline(&engine, pstore, &candidate_ids, threshold, k, &stages, min_sub_lines)?;

        if unified_pairs.is_empty() {
            println!("No duplicates found.");
        } else if json {
            print_unified_json(&unified_pairs, pstore);
        } else {
            print_pairs_text(&unified_pairs, pstore, &db);
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

    // --analyze: call graph analysis for each duplicate pair.
    if analyze && !pair_names.is_empty() {
        run_analyze(&db, &pair_names)?;
    }

    Ok(())
}

// ── Output formatting ────────────────────────────────────────────────────

/// Common interface for duplicate pair types, enabling shared output logic.
trait DupePairLike {
    fn id_a(&self) -> u64;
    fn id_b(&self) -> u64;
    fn display_score(&self) -> f32;
    fn display_tag(&self) -> String {
        String::new()
    }
}

impl DupePairLike for DupePair {
    fn id_a(&self) -> u64 { self.id_a }
    fn id_b(&self) -> u64 { self.id_b }
    fn display_score(&self) -> f32 { self.similarity }
}

impl DupePairLike for UnifiedDupePair {
    fn id_a(&self) -> u64 { self.id_a }
    fn id_b(&self) -> u64 { self.id_b }
    fn display_score(&self) -> f32 { self.score }
    fn display_tag(&self) -> String { self.tag() }
}

/// An entry in a file-grouped duplicate listing.
struct GroupEntry {
    pair_index: usize,
    name_a: String,
    name_b: String,
}

// ── Label parsing ────────────────────────────────────────────────────────

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

// ── Path alias helpers ───────────────────────────────────────────────────

use rude_intel::helpers::{apply_alias, build_path_aliases};
use rude_intel::parse::normalize_path;

// ── File grouping ────────────────────────────────────────────────────────

/// Return the index for `key` in `groups`, creating a new empty group if needed.
fn get_or_insert_idx<T>(
    key: String,
    groups: &mut Vec<(String, Vec<T>)>,
    index: &mut HashMap<String, usize>,
) -> usize {
    if let Some(&i) = index.get(&key) {
        i
    } else {
        let i = groups.len();
        index.insert(key.clone(), i);
        groups.push((key, Vec::new()));
        i
    }
}

#[expect(clippy::type_complexity, reason = "return type is consumed locally")]
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

    let refs: Vec<&str> = all_files.iter().map(String::as_str).collect();
    let (alias_map, _) = build_path_aliases(&refs);

    let aliased: Vec<(String, String)> = all_files
        .chunks_exact(2)
        .map(|c| (apply_alias(&c[0], &alias_map), apply_alias(&c[1], &alias_map)))
        .collect();

    let mut by_file: Vec<(String, Vec<GroupEntry>)> = Vec::new();
    let mut file_index: HashMap<String, usize> = HashMap::new();

    for (i, _) in ids.iter().enumerate() {
        let (ref file_a, _) = aliased[i];
        let key = if file_a.is_empty() {
            "(unknown)".to_owned()
        } else {
            file_a.clone()
        };
        let idx = get_or_insert_idx(key, &mut by_file, &mut file_index);
        by_file[idx].1.push(GroupEntry {
            pair_index: i,
            name_a: labels[i].0.name.clone(),
            name_b: labels[i].1.name.clone(),
        });
    }

    by_file.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    by_file
}

// ── Pair output (text / JSON) ────────────────────────────────────────────

fn print_pairs_text(
    pairs: &[impl DupePairLike],
    pstore: &(impl PayloadStore + ?Sized),
    db: &Path,
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
            if tag.is_empty() {
                println!("    [{score:.2}]  {} \u{2194} {}", e.name_a, e.name_b);
            } else {
                let tag_padded = format!("{tag:<12}");
                println!(
                    "    [{score:.2}] {tag_padded} {} \u{2194} {}",
                    e.name_a, e.name_b
                );
            }
        }
        println!();
    }

    let db_name = db.file_name().and_then(|n| n.to_str()).unwrap_or("db");
    eprintln!("Tip: rude context {db_name} <symbol>  to inspect.");
}

fn print_pairs_json(pairs: &[DupePair], pstore: &(impl PayloadStore + ?Sized)) {
    let output = PairsOutput {
        pairs: pairs.iter().map(|p| PairJson {
            a: label(pstore, p.id_a),
            b: label(pstore, p.id_b),
            sim: p.similarity,
        }).collect(),
    };
    println!("{}", serde_json::to_string(&output).expect("JSON serialize"));
}

fn print_unified_json(pairs: &[UnifiedDupePair], pstore: &(impl PayloadStore + ?Sized)) {
    let output = UnifiedPairsOutput {
        pairs: pairs.iter().map(|p| UnifiedPairJson {
            a: label(pstore, p.id_a),
            b: label(pstore, p.id_b),
            score: p.score,
            jaccard: p.jaccard,
            ast_match: p.ast_match,
            tag: p.tag(),
        }).collect(),
    };
    println!("{}", serde_json::to_string(&output).expect("JSON serialize"));
}

// ── Group output (AST hash mode) ─────────────────────────────────────────

fn print_groups_text(groups: &[(u64, Vec<u64>)], pstore: &(impl PayloadStore + ?Sized), db: &Path) {
    if groups.is_empty() {
        println!("No clones found.");
        return;
    }
    println!("{} clone groups found:\n", groups.len());

    let all_labels: Vec<Vec<(usize, u64, ChunkLabel)>> = groups
        .iter()
        .enumerate()
        .map(|(gi, (hash, ids))| {
            ids.iter()
                .map(|&id| (gi + 1, *hash, parse_label(pstore, id)))
                .collect()
        })
        .collect();

    let all_files: Vec<String> = all_labels
        .iter()
        .flat_map(|g| g.iter().map(|(_, _, cl)| normalize_path(&cl.file)))
        .collect();

    let refs: Vec<&str> = all_files.iter().map(String::as_str).collect();
    let (alias_map, _) = build_path_aliases(&refs);

    type FileGroup = (String, Vec<(usize, u64, String)>);
    let mut by_file: Vec<FileGroup> = Vec::new();
    let mut file_index: HashMap<String, usize> = HashMap::new();

    for group in &all_labels {
        for (group_num, hash, cl) in group {
            let normalized = normalize_path(&cl.file);
            let key = if normalized.is_empty() {
                "(unknown)".to_owned()
            } else {
                apply_alias(&normalized, &alias_map)
            };
            let idx = get_or_insert_idx(key, &mut by_file, &mut file_index);
            by_file[idx].1.push((*group_num, *hash, cl.name.clone()));
        }
    }

    by_file.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    for (file, entries) in &by_file {
        println!("  {file} ({} clones)", entries.len());
        for (group_num, hash, name) in entries {
            println!("    G{group_num} [{hash:016x}]  {name}");
        }
        println!();
    }

    let db_name = db.file_name().and_then(|n| n.to_str()).unwrap_or("db");
    eprintln!("Tip: rude context {db_name} <symbol>  to inspect.");
}

fn print_groups_json(groups: &[(u64, Vec<u64>)], pstore: &(impl PayloadStore + ?Sized)) {
    let output = GroupsOutput {
        groups: groups.iter().map(|(hash, ids)| GroupJson {
            hash: format!("{hash:016x}"),
            members: ids.iter().map(|&id| label(pstore, id)).collect(),
        }).collect(),
    };
    println!("{}", serde_json::to_string(&output).expect("JSON serialize"));
}

// ── Sub-block output ─────────────────────────────────────────────────────

fn print_sub_block_text(clones: &[SubBlockClone], pstore: &(impl PayloadStore + ?Sized)) {
    println!("\n{} sub-block clones (intra-function):\n", clones.len());

    // Collect all file paths for alias map
    let labels: Vec<(ChunkLabel, ChunkLabel)> = clones
        .iter()
        .map(|c| (parse_label(pstore, c.chunk_id_a), parse_label(pstore, c.chunk_id_b)))
        .collect();
    let all_files: Vec<String> = labels
        .iter()
        .flat_map(|(a, b)| [normalize_path(&a.file), normalize_path(&b.file)])
        .collect();
    let refs: Vec<&str> = all_files.iter().map(String::as_str).collect();
    let (alias_map, _) = build_path_aliases(&refs);

    for (i, c) in clones.iter().enumerate() {
        let file_a = apply_alias(&all_files[i * 2], &alias_map);
        let file_b = apply_alias(&all_files[i * 2 + 1], &alias_map);
        let name_a = &labels[i].0.name;
        let name_b = &labels[i].1.name;
        let lines_a = format!("L{}-{}", c.block_a_start + 1, c.block_a_end + 1);
        let lines_b = format!("L{}-{}", c.block_b_start + 1, c.block_b_end + 1);
        let body_tag = if c.body_match { " [exact]" } else { "" };
        println!("    {name_a}  ({file_a} {lines_a}) \u{2194} {name_b}  ({file_b} {lines_b}){body_tag}");
    }
    println!();
}

fn print_sub_block_json(clones: &[SubBlockClone], pstore: &(impl PayloadStore + ?Sized)) {
    let output = SubBlocksOutput {
        sub_block_clones: clones.iter().map(|c| SubBlockJson {
            a: label(pstore, c.chunk_id_a),
            a_lines: [c.block_a_start + 1, c.block_a_end + 1],
            b: label(pstore, c.chunk_id_b),
            b_lines: [c.block_b_start + 1, c.block_b_end + 1],
            body_match: c.body_match,
        }).collect(),
    };
    println!("{}", serde_json::to_string(&output).expect("JSON serialize"));
}

// ── Analyze output ───────────────────────────────────────────────────────

fn run_analyze(db: &Path, pair_names: &[(String, String)]) -> Result<()> {
    use rude_intel::dupe_analyze;
    use super::intel::load_or_build_graph;

    let graph = load_or_build_graph(db)?;

    // Resolve each name pair to graph indices.
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
