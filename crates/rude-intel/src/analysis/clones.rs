
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rayon::prelude::*;
use crate::data::minhash;
use rude_db::{PayloadStore, PayloadValue, StorageEngine};

pub struct RunStages {
    pub ast: bool,
    pub minhash: bool,
}

pub struct DupePair {
    pub id_a: u64,
    pub id_b: u64,
    pub similarity: f32,
}

pub struct UnifiedDupePair {
    pub id_a: u64,
    pub id_b: u64,
    pub score: f32,
    pub jaccard: f32,
    pub ast_match: bool,
}

impl UnifiedDupePair {
    pub fn tag(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.ast_match { parts.push("AST"); }
        if self.jaccard >= 0.5 { parts.push("Token"); }
        if parts.is_empty() { parts.push("Weak"); }
        parts.join("+")
    }
}

pub struct SubBlockClone {
    pub chunk_id_a: u64,
    pub chunk_id_b: u64,
    pub block_a_start: usize,
    pub block_a_end: usize,
    pub block_b_start: usize,
    pub block_b_end: usize,
    pub body_match: bool,
}

pub struct CloneResults {
    pub simple_pairs: Vec<DupePair>,
    pub unified_pairs: Vec<UnifiedDupePair>,
    pub sub_block_clones: Vec<SubBlockClone>,
    pub hash_groups: Vec<(u64, Vec<u64>)>,
}

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

    let mut pairs: Vec<UnifiedDupePair> = candidates.into_iter().filter_map(|(id_a, id_b)| {
        if chunks_overlap(pstore, id_a, id_b) { return None; }
        #[expect(clippy::cast_possible_truncation)]
        let jaccard = match (minhash_map.get(&id_a), minhash_map.get(&id_b)) {
            (Some(sa), Some(sb)) => minhash::jaccard_from_minhash(sa, sb) as f32,
            _ => 0.0,
        };
        let ast_match = matches!((ast_map.get(&id_a), ast_map.get(&id_b)), (Some(ha), Some(hb)) if ha == hb);
        let score = if ast_match { 1.0_f32.max(jaccard) } else { jaccard };
        (score >= threshold).then_some(UnifiedDupePair { id_a, id_b, score, jaccard, ast_match })
    }).collect();

    pairs.sort_unstable_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    pairs.truncate(k);

    // Sub-block clone detection
    let sub_clones = find_sub_block_clones(pstore, candidate_ids, min_sub_lines);

    Ok((pairs, sub_clones))
}

fn canonical_pair(a: u64, b: u64) -> (u64, u64) {
    if a < b { (a, b) } else { (b, a) }
}

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

fn parse_sub_block_entries(pstore: &(impl PayloadStore + ?Sized), id: u64) -> Vec<(u64, usize, usize)> {
    let Some(payload) = pstore.get_payload(id).ok().flatten() else { return Vec::new() };
    let Some(PayloadValue::StringList(hashes)) = payload.custom.get("sub_block_hashes") else { return Vec::new() };
    hashes.iter().filter_map(|s| {
        let (hash_hex, range) = s.split_once(':')?;
        let (start_s, end_s) = range.split_once('-')?;
        Some((u64::from_str_radix(hash_hex, 16).ok()?, start_s.parse().ok()?, end_s.parse().ok()?))
    }).collect()
}

fn build_chunk_ranges(
    pstore: &(impl PayloadStore + ?Sized),
    ids: &[u64],
) -> HashMap<u64, (String, i64, i64)> {
    ids.iter()
        .filter_map(|&id| get_chunk_source_range(pstore, id).map(|r| (id, r)))
        .collect()
}

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

    let mut clones: Vec<SubBlockClone> = hash_groups.values().filter(|e| e.len() >= 2)
        .flat_map(|entries| {
            let n = entries.len();
            let mut out = Vec::new();
            for i in 0..n {
                for j in (i + 1)..n {
                    let (id_a, sa, ea) = entries[i];
                    let (id_b, sb, eb) = entries[j];
                    if id_a == id_b { continue; }
                    if ea.saturating_sub(sa) < min_sub_lines || eb.saturating_sub(sb) < min_sub_lines { continue; }
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

fn containment_remove_idx(
    ranges: &HashMap<u64, (String, i64, i64)>,
    id_a: u64, i: usize,
    id_b: u64, j: usize,
) -> Option<usize> {
    let (a, b) = (ranges.get(&id_a)?, ranges.get(&id_b)?);
    if a.0 != b.0 { return None; }
    let (a_size, b_size) = (a.2 - a.1, b.2 - b.1);
    if (a.1 <= b.1 && a.2 >= b.2) || (a_size > b_size && a.1 <= b.1) { Some(i) }
    else if (b.1 <= a.1 && b.2 >= a.2) || (b_size > a_size && b.1 <= a.1) { Some(j) }
    else { None }
}

#[inline]
fn chunk_contains(ranges: &HashMap<u64, (String, i64, i64)>, id_a: u64, id_b: u64) -> bool {
    containment_remove_idx(ranges, id_a, 0, id_b, 1).is_some()
}

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
            if let Some(idx) = containment_remove_idx(ranges, id_a, i, id_b, j) {
                to_remove.push(idx);
            }
        }
    }
    to_remove.sort_unstable();
    to_remove.dedup();
    for &idx in to_remove.iter().rev() { entries.swap_remove(idx); }
}

fn is_test_chunk(pstore: &(impl PayloadStore + ?Sized), id: u64) -> bool {
    let Some(payload) = pstore.get_payload(id).ok().flatten() else { return false };
    if crate::graph::is_test_path(&payload.source) { return true; }
    // "[function] test_foo" — name starts with test_ in the kind header.
    pstore.get_text(id).ok().flatten()
        .is_some_and(|t| t.lines().next().unwrap_or("").contains("] test_"))
}

fn get_chunk_source_range(pstore: &(impl PayloadStore + ?Sized), id: u64) -> Option<(String, i64, i64)> {
    let p = pstore.get_payload(id).ok()??;
    match (p.custom.get("start_line"), p.custom.get("end_line")) {
        (Some(PayloadValue::Integer(s)), Some(PayloadValue::Integer(e))) => Some((p.source.clone(), *s, *e)),
        _ => None,
    }
}

pub fn chunks_overlap(pstore: &(impl PayloadStore + ?Sized), id_a: u64, id_b: u64) -> bool {
    let (Some((fa, sa, ea)), Some((fb, sb, eb))) =
        (get_chunk_source_range(pstore, id_a), get_chunk_source_range(pstore, id_b)) else { return false };
    fa == fb && sa <= eb && sb <= ea
}

pub fn chunk_lines(pstore: &(impl PayloadStore + ?Sized), id: u64) -> usize {
    let Some((_, s, e)) = get_chunk_source_range(pstore, id) else { return 0 };
    #[expect(clippy::cast_sign_loss)]
    { (e - s + 1) as usize }
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
    let mut groups: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut missing = 0u32;
    for &id in candidate_ids {
        match get_hash(pstore, id, hash_key) {
            Some(k) => groups.entry(k).or_default().push(id),
            None => missing += 1,
        }
    }
    if missing > 0 { eprintln!("  {hash_key}: {missing} chunks without {hash_key}"); }
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

fn finalize_pairs(pairs: &mut Vec<DupePair>, pstore: &(impl PayloadStore + ?Sized), k: usize) {
    pairs.retain(|p| !chunks_overlap(pstore, p.id_a, p.id_b));
    pairs.sort_unstable_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
    pairs.truncate(k);
}

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
