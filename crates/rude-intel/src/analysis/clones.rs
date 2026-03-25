//! Clone detection algorithms — finds duplicate code chunks in a database.
//!
//! Two detection signals:
//! - **AST hash**: structural clones ignoring identifier names (Type-1/2)
//! - **MinHash Jaccard**: token-based near-duplicate detection (Type-1~3)
//!
//! Two execution modes:
//! - Single-signal fast path (one signal only, user threshold)
//! - Unified pipeline (all signals: Filter → Verify, weighted scoring)

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rayon::prelude::*;
use crate::data::minhash;
use rude_db::{PayloadStore, PayloadValue, StorageEngine};

// ── Configuration ────────────────────────────────────────────────────────

/// Which detection stages to enable.
pub struct RunStages {
    pub ast: bool,
    pub minhash: bool,
}

// ── Result types ─────────────────────────────────────────────────────────

/// A pair of duplicate chunks with similarity score.
pub struct DupePair {
    pub id_a: u64,
    pub id_b: u64,
    pub similarity: f32,
}

/// Result of the unified multi-signal pipeline.
pub struct UnifiedDupePair {
    pub id_a: u64,
    pub id_b: u64,
    pub score: f32,
    pub jaccard: f32,
    pub ast_match: bool,
}

impl UnifiedDupePair {
    /// Build a tag string like "AST", "Token", "AST+Token", etc.
    pub fn tag(&self) -> String {
        let mut parts = Vec::new();
        if self.ast_match {
            parts.push("AST");
        }
        if self.jaccard >= 0.5 {
            parts.push("Token");
        }
        if parts.is_empty() {
            parts.push("Weak");
        }
        parts.join("+")
    }
}

/// A pair of sub-blocks from different chunks that share the same AST hash.
pub struct SubBlockClone {
    pub chunk_id_a: u64,
    pub chunk_id_b: u64,
    pub block_a_start: usize,
    pub block_a_end: usize,
    pub block_b_start: usize,
    pub block_b_end: usize,
    pub body_match: bool,
}

/// Combined results from the clone detection pipeline.
pub struct CloneResults {
    pub simple_pairs: Vec<DupePair>,
    pub unified_pairs: Vec<UnifiedDupePair>,
    pub sub_block_clones: Vec<SubBlockClone>,
    /// Hash-based groups: (hash, Vec<member_ids>).
    pub hash_groups: Vec<(u64, Vec<u64>)>,
}

// ── Candidate collection ─────────────────────────────────────────────────

/// Collect candidate IDs from the vector store, applying test/line filters.
pub fn collect_filtered_ids(
    engine: &StorageEngine,
    pstore: &(impl PayloadStore + ?Sized),
    exclude_tests: bool,
    min_lines: usize,
) -> Vec<u64> {
    let mut ids: Vec<u64> = engine.all_ids().unwrap_or_default();
    if exclude_tests {
        ids.retain(|id| !is_test_chunk(pstore, *id));
    }
    if min_lines > 0 {
        ids.retain(|id| chunk_lines(pstore, *id) >= min_lines);
    }
    ids
}

// ── Single-signal: AST hash groups ───────────────────────────────────────

/// Find clone groups by AST hash, sorted by size desc, truncated to `k`.
pub fn find_hash_groups(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    hash_key: &str,
    k: usize,
) -> Vec<(u64, Vec<u64>)> {
    let mut hash_groups = collect_hash_groups(pstore, candidate_ids, hash_key);
    for ids in hash_groups.values_mut().filter(|ids| ids.len() > 1) {
        remove_overlapping_chunks(pstore, ids);
    }
    let mut groups: Vec<(u64, Vec<u64>)> = hash_groups.into_iter()
        .filter(|(_, ids)| ids.len() > 1).collect();
    groups.sort_unstable_by(|a, b| b.1.len().cmp(&a.1.len()));
    groups.truncate(k);
    groups
}

/// Find duplicate pairs by MinHash Jaccard similarity, sorted desc, truncated to `k`.
pub fn find_minhash_pairs(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    threshold: f32,
    k: usize,
) -> Vec<DupePair> {
    let entries = collect_minhash_entries(pstore, candidate_ids);
    if entries.len() < 2 { return Vec::new(); }
    let mut pairs = minhash_all_pairs(&entries, f64::from(threshold));
    finalize_pairs(&mut pairs, pstore, k);
    pairs
}

// ── Unified multi-signal pipeline ────────────────────────────────────────

/// Run the full unified pipeline: Filter (AST+MinHash) → Verify → Score.
///
/// Returns `(unified_pairs, sub_block_clones)`.
pub fn run_unified_pipeline(
    _engine: &StorageEngine,
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    threshold: f32,
    k: usize,
    stages: &RunStages,
    min_sub_lines: usize,
) -> Result<(Vec<UnifiedDupePair>, Vec<SubBlockClone>)> {
    // Stage 1: Filter (collect candidate pairs)
    let mut candidates: HashSet<(u64, u64)> = HashSet::new();

    if stages.ast {
        stage1_ast_hash(pstore, candidate_ids, &mut candidates);
    }
    if stages.minhash {
        stage1_minhash(pstore, candidate_ids, &mut candidates);
    }

    eprintln!("Stage 1: {} candidate pairs", candidates.len());

    if candidates.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    // Stage 2: Verify — compute all signals for each candidate pair.
    let minhash_map: HashMap<u64, Vec<u64>> = candidate_ids.iter()
        .filter_map(|&id| get_minhash(pstore, id).map(|sig| (id, sig))).collect();
    let ast_map: HashMap<u64, u64> = candidate_ids.iter()
        .filter_map(|&id| get_hash(pstore, id, "ast_hash").map(|h| (id, h))).collect();

    let mut pairs: Vec<UnifiedDupePair> = candidates.into_iter()
        .filter_map(|(id_a, id_b)| {
            if chunks_overlap(pstore, id_a, id_b) { return None; }
            #[expect(clippy::cast_possible_truncation)]
            let jaccard = match (minhash_map.get(&id_a), minhash_map.get(&id_b)) {
                (Some(sa), Some(sb)) => minhash::jaccard_from_minhash(sa, sb) as f32,
                _ => 0.0,
            };
            let ast_match = matches!((ast_map.get(&id_a), ast_map.get(&id_b)), (Some(ha), Some(hb)) if ha == hb);
            let score = if ast_match { 1.0_f32.max(jaccard) } else { jaccard };
            (score >= threshold).then_some(UnifiedDupePair { id_a, id_b, score, jaccard, ast_match })
        })
        .collect();

    pairs.sort_unstable_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    pairs.truncate(k);

    // Sub-block clone detection
    let sub_clones = find_sub_block_clones(pstore, candidate_ids, min_sub_lines);

    Ok((pairs, sub_clones))
}

// ── Pipeline stages ──────────────────────────────────────────────────────

/// Normalize a pair so the smaller ID comes first (canonical form for dedup).
fn canonical_pair(a: u64, b: u64) -> (u64, u64) {
    if a < b { (a, b) } else { (b, a) }
}

/// Stage 1a: Group by AST hash → all intra-group pairs become candidates.
fn stage1_ast_hash(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    candidates: &mut HashSet<(u64, u64)>,
) {
    let hash_groups = collect_hash_groups(pstore, candidate_ids, "ast_hash");
    let mut ast_pairs = 0usize;
    for ids in hash_groups.values().filter(|ids| ids.len() > 1) {
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                candidates.insert(canonical_pair(ids[i], ids[j]));
                ast_pairs += 1;
            }
        }
    }
    eprintln!("  AST hash: {ast_pairs} candidate pairs");
}

/// Stage 1b: MinHash Jaccard with low threshold (0.3) for broad candidate collection.
fn stage1_minhash(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    candidates: &mut HashSet<(u64, u64)>,
) {
    let entries = collect_minhash_entries(pstore, candidate_ids);
    if entries.len() < 2 {
        eprintln!("  MinHash: not enough chunks ({})", entries.len());
        return;
    }
    let pairs = minhash_all_pairs(&entries, 0.3);
    eprintln!("  MinHash: {} candidate pairs (threshold=0.30)", pairs.len());
    candidates.extend(pairs.into_iter().map(|p| canonical_pair(p.id_a, p.id_b)));
}

// ── Sub-block clone detection ────────────────────────────────────────────

/// Parse `sub_block_hashes` payload: `["<hex_hash>:<start>-<end>", ...]`
fn parse_sub_block_entries(pstore: &(impl PayloadStore + ?Sized), id: u64) -> Vec<(u64, usize, usize)> {
    let Some(payload) = pstore.get_payload(id).ok().flatten() else { return Vec::new() };
    let Some(PayloadValue::StringList(hashes)) = payload.custom.get("sub_block_hashes") else { return Vec::new() };
    hashes.iter().filter_map(|s| {
        let (hash_hex, range) = s.split_once(':')?;
        let (start_s, end_s) = range.split_once('-')?;
        Some((u64::from_str_radix(hash_hex, 16).ok()?, start_s.parse().ok()?, end_s.parse().ok()?))
    }).collect()
}

/// Build a map of `id → (source_file, start_line, end_line)` for containment checks.
fn build_chunk_ranges(
    pstore: &(impl PayloadStore + ?Sized),
    ids: &[u64],
) -> HashMap<u64, (String, i64, i64)> {
    ids.iter()
        .filter_map(|&id| {
            let p = pstore.get_payload(id).ok()??;
            let (Some(PayloadValue::Integer(s)), Some(PayloadValue::Integer(e))) =
                (p.custom.get("start_line"), p.custom.get("end_line"))
            else { return None };
            Some((id, (p.source.clone(), *s, *e)))
        })
        .collect()
}

/// Find sub-block clones across all candidate chunks.
fn find_sub_block_clones(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    min_sub_lines: usize,
) -> Vec<SubBlockClone> {
    let chunk_ranges = build_chunk_ranges(pstore, candidate_ids);

    // Build per-hash groups from sub_block_hashes payload fields.
    let mut hash_groups: HashMap<u64, Vec<(u64, usize, usize)>> = HashMap::new();
    for &id in candidate_ids {
        for (ast_hash, start, end) in parse_sub_block_entries(pstore, id) {
            hash_groups.entry(ast_hash).or_default().push((id, start, end));
        }
    }

    // Deduplicate: keep the most-specific (smallest) chunk per file+block.
    for entries in hash_groups.values_mut() {
        deduplicate_contained_entries(entries, &chunk_ranges);
    }

    let mut clones: Vec<SubBlockClone> = hash_groups
        .values()
        .filter(|e| e.len() >= 2)
        .flat_map(|entries| {
            let mut out = Vec::new();
            for i in 0..entries.len() {
                for j in (i + 1)..entries.len() {
                    let (id_a, sa, ea) = entries[i];
                    let (id_b, sb, eb) = entries[j];
                    if id_a == id_b { continue; }
                    // Skip very small blocks — too noisy
                    if ea.saturating_sub(sa) < min_sub_lines
                        || eb.saturating_sub(sb) < min_sub_lines { continue; }
                    if same_file(pstore, id_a, id_b) && ranges_overlap(sa, ea, sb, eb) { continue; }
                    if chunk_contains(&chunk_ranges, id_a, id_b) { continue; }
                    out.push(SubBlockClone {
                        chunk_id_a: id_a, chunk_id_b: id_b,
                        block_a_start: sa, block_a_end: ea,
                        block_b_start: sb, block_b_end: eb,
                        body_match: false,
                    });
                }
            }
            out
        })
        .collect();

    clones.sort_by_key(|c| std::cmp::Reverse(c.block_a_end.saturating_sub(c.block_a_start)));
    clones.truncate(50);
    clones
}

/// Check if one chunk fully contains the other (same file, line range containment).
fn chunk_contains(
    ranges: &HashMap<u64, (String, i64, i64)>,
    id_a: u64,
    id_b: u64,
) -> bool {
    let (Some(a), Some(b)) = (ranges.get(&id_a), ranges.get(&id_b)) else {
        return false;
    };
    a.0 == b.0 && ((a.1 <= b.1 && a.2 >= b.2) || (b.1 <= a.1 && b.2 >= a.2))
}

/// Remove entries from parent chunks when a more specific child chunk exists.
///
/// For each hash group, if two entries are in the same file and one chunk's
/// line range fully contains the other (or is larger and starts earlier),
/// keep only the smaller (more specific) one.
fn deduplicate_contained_entries(
    entries: &mut Vec<(u64, usize, usize)>,
    ranges: &HashMap<u64, (String, i64, i64)>,
) {
    if entries.len() < 2 { return; }
    let mut to_remove = Vec::new();
    for i in 0..entries.len() {
        for j in (i + 1)..entries.len() {
            let (id_a, _, _) = entries[i];
            let (id_b, _, _) = entries[j];
            if id_a == id_b { continue; }
            let (Some(a), Some(b)) = (ranges.get(&id_a), ranges.get(&id_b)) else { continue };
            if a.0 != b.0 { continue; }
            // Same file: remove whichever chunk is the larger parent
            let a_size = a.2 - a.1;
            let b_size = b.2 - b.1;
            if (a.1 <= b.1 && a.2 >= b.2) || (a_size > b_size && a.1 <= b.1) {
                to_remove.push(i); // A is parent/larger, remove A
            } else if (b.1 <= a.1 && b.2 >= a.2) || (b_size > a_size && b.1 <= a.1) {
                to_remove.push(j); // B is parent/larger, remove B
            }
        }
    }
    to_remove.sort_unstable();
    to_remove.dedup();
    for &idx in to_remove.iter().rev() { entries.swap_remove(idx); }
}

// ── Shared helpers ───────────────────────────────────────────────────────

/// Payload-based test detection — uses shared `is_test_path` + first-line `[test]` marker.
fn is_test_chunk(pstore: &(impl PayloadStore + ?Sized), id: u64) -> bool {
    let Some(payload) = pstore.get_payload(id).ok().flatten() else { return false };
    if crate::graph::is_test_path(&payload.source) { return true; }
    // "[function] test_foo" — name starts with test_ in the kind header.
    pstore.get_text(id).ok().flatten()
        .is_some_and(|t| t.lines().next().unwrap_or("").contains("] test_"))
}

/// Check if two chunks overlap in the same file (parent/child relationship).
pub fn chunks_overlap(pstore: &(impl PayloadStore + ?Sized), id_a: u64, id_b: u64) -> bool {
    let Some(pa) = pstore.get_payload(id_a).ok().flatten() else { return false };
    let Some(pb) = pstore.get_payload(id_b).ok().flatten() else { return false };
    if pa.source != pb.source { return false; }
    let (Some(PayloadValue::Integer(sa)), Some(PayloadValue::Integer(ea))) =
        (pa.custom.get("start_line"), pa.custom.get("end_line")) else { return false };
    let (Some(PayloadValue::Integer(sb)), Some(PayloadValue::Integer(eb))) =
        (pb.custom.get("start_line"), pb.custom.get("end_line")) else { return false };
    *sa <= *eb && *sb <= *ea
}

/// Get the number of lines in a chunk from its custom fields.
pub fn chunk_lines(pstore: &(impl PayloadStore + ?Sized), id: u64) -> usize {
    let Some(payload) = pstore.get_payload(id).ok().flatten() else { return 0 };
    let (Some(PayloadValue::Integer(s)), Some(PayloadValue::Integer(e))) =
        (payload.custom.get("start_line"), payload.custom.get("end_line"))
    else { return 0 };
    #[expect(clippy::cast_sign_loss)]
    { (e - s + 1) as usize }
}

/// Get an integer custom field from a chunk's payload, reinterpreted as u64.
fn get_hash(pstore: &(impl PayloadStore + ?Sized), id: u64, key: &str) -> Option<u64> {
    let payload = pstore.get_payload(id).ok()??;
    match payload.custom.get(key)? {
        #[expect(clippy::cast_sign_loss, reason = "hash bits reinterpreted")]
        PayloadValue::Integer(v) => Some(*v as u64),
        _ => None,
    }
}

fn get_minhash(pstore: &(impl PayloadStore + ?Sized), id: u64) -> Option<Vec<u64>> {
    let payload = pstore.get_payload(id).ok()??;
    match payload.custom.get("minhash")? {
        PayloadValue::String(hex) => minhash::minhash_from_hex(hex),
        _ => None,
    }
}

/// Generic grouping: iterate IDs, extract a u64 key via `f`, bucket into a HashMap.
/// Returns (groups, missing_count).
fn group_by_key<F>(candidate_ids: &[u64], mut f: F) -> (HashMap<u64, Vec<u64>>, u32)
where
    F: FnMut(u64) -> Option<u64>,
{
    let mut groups: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut missing = 0u32;
    for &id in candidate_ids {
        match f(id) {
            Some(key) => groups.entry(key).or_default().push(id),
            None => missing += 1,
        }
    }
    (groups, missing)
}

fn collect_hash_groups(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    hash_key: &str,
) -> HashMap<u64, Vec<u64>> {
    let (groups, no_hash) = group_by_key(candidate_ids, |id| get_hash(pstore, id, hash_key));
    if no_hash > 0 {
        eprintln!("  {hash_key}: {no_hash} chunks without {hash_key}");
    }
    groups
}

fn collect_minhash_entries(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
) -> Vec<(u64, Vec<u64>)> {
    let entries: Vec<_> = candidate_ids.iter()
        .filter_map(|&id| get_minhash(pstore, id).map(|sig| (id, sig)))
        .collect();
    let missing = candidate_ids.len() - entries.len();
    if missing > 0 { eprintln!("  MinHash: {missing} chunks without minhash"); }
    entries
}

/// All-pairs MinHash Jaccard comparison (parallelized).
fn minhash_all_pairs(entries: &[(u64, Vec<u64>)], threshold: f64) -> Vec<DupePair> {
    let n = entries.len();
    (0..n)
        .into_par_iter()
        .flat_map_iter(|i| {
            let (id_a, sig_a) = &entries[i];
            ((i + 1)..n).filter_map(move |j| {
                let (id_b, sig_b) = &entries[j];
                let sim = minhash::jaccard_from_minhash(sig_a, sig_b);
                #[expect(clippy::cast_possible_truncation)]
                (sim >= threshold).then_some(DupePair {
                    id_a: *id_a,
                    id_b: *id_b,
                    similarity: sim as f32,
                })
            })
        })
        .collect()
}

/// Sort pairs by similarity descending, filter overlapping chunks, truncate to top `k`.
fn finalize_pairs(pairs: &mut Vec<DupePair>, pstore: &(impl PayloadStore + ?Sized), k: usize) {
    pairs.retain(|p| !chunks_overlap(pstore, p.id_a, p.id_b));
    pairs.sort_unstable_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    pairs.truncate(k);
}

/// Remove overlapping chunks within a group (keep the larger span).
fn remove_overlapping_chunks(pstore: &(impl PayloadStore + ?Sized), ids: &mut Vec<u64>) {
    let mut i = 0;
    while i < ids.len() {
        let mut j = i + 1;
        while j < ids.len() {
            if chunks_overlap(pstore, ids[i], ids[j]) {
                if chunk_lines(pstore, ids[i]) >= chunk_lines(pstore, ids[j]) {
                    ids.swap_remove(j);
                } else {
                    ids.swap_remove(i);
                    j = i + 1;
                    continue;
                }
            } else {
                j += 1;
            }
        }
        i += 1;
    }
}

fn same_file(pstore: &(impl PayloadStore + ?Sized), id_a: u64, id_b: u64) -> bool {
    let fa = pstore.get_payload(id_a).ok().flatten().map(|p| p.source);
    let fb = pstore.get_payload(id_b).ok().flatten().map(|p| p.source);
    fa.is_some() && fa == fb
}

pub(crate) fn ranges_overlap(s1: usize, e1: usize, s2: usize, e2: usize) -> bool {
    s1 <= e2 && s2 <= e1
}
