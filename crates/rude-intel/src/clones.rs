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
use crate::minhash;
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

/// Find clone groups by AST hash, with overlap removal.
///
/// Returns groups sorted by size descending, truncated to `k`.
pub fn find_hash_groups(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    hash_key: &str,
    k: usize,
) -> Vec<(u64, Vec<u64>)> {
    let mut hash_groups = collect_hash_groups(pstore, candidate_ids, hash_key);

    // Remove overlapping chunks within each group (keep the larger span)
    for ids in hash_groups.values_mut() {
        if ids.len() > 1 {
            remove_overlapping_chunks(pstore, ids);
        }
    }

    let mut clone_groups: Vec<(u64, Vec<u64>)> = hash_groups
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .collect();

    clone_groups.sort_unstable_by(|a, b| b.1.len().cmp(&a.1.len()));
    clone_groups.truncate(k);
    clone_groups
}

// ── Single-signal: MinHash Jaccard ───────────────────────────────────────

/// Find duplicate pairs by MinHash Jaccard similarity.
///
/// Returns pairs above `threshold`, overlap-filtered, sorted desc, truncated to `k`.
pub fn find_minhash_pairs(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    threshold: f32,
    k: usize,
) -> Vec<DupePair> {
    let entries = collect_minhash_entries(pstore, candidate_ids);

    if entries.len() < 2 {
        return Vec::new();
    }

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

    // Stage 2: Verify (compute all signals for each candidate)
    let minhash_map: HashMap<u64, Vec<u64>> = candidate_ids
        .iter()
        .filter_map(|&id| get_minhash(pstore, id).map(|sig| (id, sig)))
        .collect();

    let ast_map: HashMap<u64, u64> = candidate_ids
        .iter()
        .filter_map(|&id| get_hash(pstore, id, "ast_hash").map(|h| (id, h)))
        .collect();

    let candidate_vec: Vec<(u64, u64)> = candidates.into_iter().collect();

    let mut pairs: Vec<UnifiedDupePair> = candidate_vec
        .iter()
        .filter_map(|&(id_a, id_b)| {
            if chunks_overlap(pstore, id_a, id_b) {
                return None;
            }

            let jaccard = match (minhash_map.get(&id_a), minhash_map.get(&id_b)) {
                (Some(sig_a), Some(sig_b)) => {
                    #[expect(clippy::cast_possible_truncation)]
                    let j = minhash::jaccard_from_minhash(sig_a, sig_b) as f32;
                    j
                }
                _ => 0.0,
            };

            let ast_match = match (ast_map.get(&id_a), ast_map.get(&id_b)) {
                (Some(ha), Some(hb)) => ha == hb,
                _ => false,
            };

            let score = if ast_match {
                1.0_f32.max(jaccard)
            } else {
                jaccard
            };

            (score >= threshold).then_some(UnifiedDupePair {
                id_a,
                id_b,
                score,
                jaccard,
                ast_match,
            })
        })
        .collect();

    pairs.sort_unstable_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    pairs.truncate(k);

    // Sub-block clone detection
    let sub_clones = find_sub_block_clones(pstore, candidate_ids, min_sub_lines);

    Ok((pairs, sub_clones))
}

// ── Pipeline stages ──────────────────────────────────────────────────────

/// Stage 1a: Group by AST hash → all intra-group pairs become candidates.
fn stage1_ast_hash(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    candidates: &mut HashSet<(u64, u64)>,
) {
    let hash_groups = collect_hash_groups(pstore, candidate_ids, "ast_hash");

    let mut ast_pairs = 0usize;
    for ids in hash_groups.values() {
        if ids.len() > 1 {
            for i in 0..ids.len() {
                for j in (i + 1)..ids.len() {
                    let pair = if ids[i] < ids[j] {
                        (ids[i], ids[j])
                    } else {
                        (ids[j], ids[i])
                    };
                    candidates.insert(pair);
                    ast_pairs += 1;
                }
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

    let n = entries.len();
    if n < 2 {
        eprintln!("  MinHash: not enough chunks ({n})");
        return;
    }

    let pairs = minhash_all_pairs(&entries, 0.3);
    let minhash_pairs: Vec<(u64, u64)> = pairs
        .into_iter()
        .map(|p| {
            if p.id_a < p.id_b {
                (p.id_a, p.id_b)
            } else {
                (p.id_b, p.id_a)
            }
        })
        .collect();

    eprintln!(
        "  MinHash: {} candidate pairs (threshold=0.30)",
        minhash_pairs.len()
    );
    candidates.extend(minhash_pairs);
}

// ── Sub-block clone detection ────────────────────────────────────────────

/// Parse `sub_block_hashes` payload field: `["<hex_ast_hash>:<start>-<end>", ...]`
fn parse_sub_block_entries(pstore: &(impl PayloadStore + ?Sized), id: u64) -> Vec<(u64, usize, usize)> {
    let Some(payload) = pstore.get_payload(id).ok().flatten() else {
        return Vec::new();
    };
    let Some(PayloadValue::StringList(hashes)) = payload.custom.get("sub_block_hashes") else {
        return Vec::new();
    };
    hashes
        .iter()
        .filter_map(|s| {
            let (hash_hex, range) = s.split_once(':')?;
            let (start_s, end_s) = range.split_once('-')?;
            let hash = u64::from_str_radix(hash_hex, 16).ok()?;
            let start = start_s.parse().ok()?;
            let end = end_s.parse().ok()?;
            Some((hash, start, end))
        })
        .collect()
}

/// Find sub-block clones across all candidate chunks.
fn find_sub_block_clones(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    min_sub_lines: usize,
) -> Vec<SubBlockClone> {
    // Pre-compute chunk line ranges for containment checks
    let chunk_ranges: HashMap<u64, (String, i64, i64)> = candidate_ids
        .iter()
        .filter_map(|&id| {
            let p = pstore.get_payload(id).ok()??;
            let s = match p.custom.get("start_line") {
                Some(PayloadValue::Integer(v)) => *v,
                _ => return None,
            };
            let e = match p.custom.get("end_line") {
                Some(PayloadValue::Integer(v)) => *v,
                _ => return None,
            };
            Some((id, (p.source.clone(), s, e)))
        })
        .collect();

    let mut hash_groups: HashMap<u64, Vec<(u64, usize, usize)>> = HashMap::new();
    for &id in candidate_ids {
        for (ast_hash, start, end) in parse_sub_block_entries(pstore, id) {
            hash_groups
                .entry(ast_hash)
                .or_default()
                .push((id, start, end));
        }
    }

    // Deduplicate: for each hash group, keep only the smallest (most specific)
    // chunk per file+block combination. This removes entries from parent chunks
    // (e.g. full impl block) when a child chunk (e.g. individual method) exists.
    for entries in hash_groups.values_mut() {
        deduplicate_contained_entries(entries, &chunk_ranges);
    }

    let mut clones = Vec::new();
    for entries in hash_groups.values() {
        if entries.len() < 2 {
            continue;
        }
        for i in 0..entries.len() {
            for j in (i + 1)..entries.len() {
                let (id_a, sa, ea) = entries[i];
                let (id_b, sb, eb) = entries[j];
                if id_a == id_b {
                    continue;
                }
                // Skip very small blocks — too noisy
                if ea.saturating_sub(sa) < min_sub_lines || eb.saturating_sub(sb) < min_sub_lines {
                    continue;
                }
                if same_file(pstore, id_a, id_b) && ranges_overlap(sa, ea, sb, eb) {
                    continue;
                }
                // Skip if one chunk fully contains the other in the same file
                if chunk_contains(&chunk_ranges, id_a, id_b) {
                    continue;
                }
                clones.push(SubBlockClone {
                    chunk_id_a: id_a,
                    chunk_id_b: id_b,
                    block_a_start: sa,
                    block_a_end: ea,
                    block_b_start: sb,
                    block_b_end: eb,
                    body_match: false,
                });
            }
        }
    }

    clones.sort_by(|a, b| {
        let size_a = a.block_a_end.saturating_sub(a.block_a_start);
        let size_b = b.block_a_end.saturating_sub(b.block_a_start);
        size_b.cmp(&size_a)
    });
    clones.truncate(50);
    clones
}

/// Remove entries from parent chunks when a more specific child chunk exists.
///
/// For each hash group, if two entries are in the same file and one chunk's
/// line range fully contains the other, keep only the smaller (more specific) one.
fn deduplicate_contained_entries(
    entries: &mut Vec<(u64, usize, usize)>,
    ranges: &HashMap<u64, (String, i64, i64)>,
) {
    if entries.len() < 2 {
        return;
    }
    let mut to_remove = Vec::new();
    for i in 0..entries.len() {
        for j in (i + 1)..entries.len() {
            let (id_a, _, _) = entries[i];
            let (id_b, _, _) = entries[j];
            if id_a == id_b {
                continue;
            }
            if let (Some(a), Some(b)) = (ranges.get(&id_a), ranges.get(&id_b))
                && a.0 == b.0 {
                    // Same file: remove the larger (parent) chunk entry
                    let a_size = a.2 - a.1;
                    let b_size = b.2 - b.1;
                    if a.1 <= b.1 && a.2 >= b.2 {
                        to_remove.push(i); // A contains B, remove A
                    } else if b.1 <= a.1 && b.2 >= a.2 {
                        to_remove.push(j); // B contains A, remove B
                    } else if a_size > b_size && a.1 <= b.1 {
                        to_remove.push(i);
                    } else if b_size > a_size && b.1 <= a.1 {
                        to_remove.push(j);
                    }
                }
        }
    }
    to_remove.sort_unstable();
    to_remove.dedup();
    for &idx in to_remove.iter().rev() {
        entries.swap_remove(idx);
    }
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
    if a.0 != b.0 {
        return false;
    }
    // A contains B, or B contains A
    (a.1 <= b.1 && a.2 >= b.2) || (b.1 <= a.1 && b.2 >= a.2)
}

// ── Shared helpers ───────────────────────────────────────────────────────

/// Payload-based test detection — uses shared `is_test_path` + first-line `[test]` marker.
fn is_test_chunk(pstore: &(impl PayloadStore + ?Sized), id: u64) -> bool {
    let Some(payload) = pstore.get_payload(id).ok().flatten() else {
        return false;
    };
    if crate::graph::is_test_path(&payload.source) {
        return true;
    }
    // Check if chunk text's first line contains [test] marker (set by chunker).
    if let Ok(Some(text)) = pstore.get_text(id) {
        let first_line = text.lines().next().unwrap_or("");
        // "[function] test_foo" — name starts with test_ in the kind header.
        if first_line.contains("] test_") {
            return true;
        }
    }
    false
}

/// Check if two chunks overlap in the same file (parent/child relationship).
pub fn chunks_overlap(pstore: &(impl PayloadStore + ?Sized), id_a: u64, id_b: u64) -> bool {
    let (Some(pa), Some(pb)) = (
        pstore.get_payload(id_a).ok().flatten(),
        pstore.get_payload(id_b).ok().flatten(),
    ) else {
        return false;
    };
    if pa.source != pb.source {
        return false;
    }
    let (Some(PayloadValue::Integer(sa)), Some(PayloadValue::Integer(ea))) =
        (pa.custom.get("start_line"), pa.custom.get("end_line"))
    else {
        return false;
    };
    let (Some(PayloadValue::Integer(sb)), Some(PayloadValue::Integer(eb))) =
        (pb.custom.get("start_line"), pb.custom.get("end_line"))
    else {
        return false;
    };
    *sa <= *eb && *sb <= *ea
}

/// Get the number of lines in a chunk from its custom fields.
pub fn chunk_lines(pstore: &(impl PayloadStore + ?Sized), id: u64) -> usize {
    let Some(payload) = pstore.get_payload(id).ok().flatten() else {
        return 0;
    };
    let start = match payload.custom.get("start_line") {
        Some(PayloadValue::Integer(v)) => *v,
        _ => return 0,
    };
    let end = match payload.custom.get("end_line") {
        Some(PayloadValue::Integer(v)) => *v,
        _ => return 0,
    };
    #[expect(clippy::cast_sign_loss)]
    let lines = (end - start + 1) as usize;
    lines
}

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

fn collect_hash_groups(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
    hash_key: &str,
) -> HashMap<u64, Vec<u64>> {
    let mut hash_groups: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut no_hash = 0u32;

    for &id in candidate_ids {
        if let Some(hash) = get_hash(pstore, id, hash_key) {
            hash_groups.entry(hash).or_default().push(id);
        } else {
            no_hash += 1;
        }
    }

    if no_hash > 0 {
        eprintln!("  {hash_key}: {no_hash} chunks without {hash_key}");
    }
    hash_groups
}

fn collect_minhash_entries(
    pstore: &(impl PayloadStore + ?Sized),
    candidate_ids: &[u64],
) -> Vec<(u64, Vec<u64>)> {
    let mut entries: Vec<(u64, Vec<u64>)> = Vec::new();
    let mut no_minhash = 0u32;

    for &id in candidate_ids {
        if let Some(sig) = get_minhash(pstore, id) {
            entries.push((id, sig));
        } else {
            no_minhash += 1;
        }
    }

    if no_minhash > 0 {
        eprintln!("  MinHash: {no_minhash} chunks without minhash");
    }
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
    let file_a = pstore
        .get_payload(id_a)
        .ok()
        .flatten()
        .map(|p| p.source);
    let file_b = pstore
        .get_payload(id_b)
        .ok()
        .flatten()
        .map(|p| p.source);
    file_a.is_some() && file_a == file_b
}

pub(crate) fn ranges_overlap(s1: usize, e1: usize, s2: usize, e2: usize) -> bool {
    s1 <= e2 && s2 <= e1
}
