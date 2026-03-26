
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rayon::prelude::*;
use crate::data::minhash;
use crate::data::parse::ParsedChunk;

pub struct RunStages {
    pub ast: bool,
    pub minhash: bool,
}

pub struct DupePair {
    pub idx_a: usize,
    pub idx_b: usize,
    pub similarity: f32,
}

pub struct UnifiedDupePair {
    pub idx_a: usize,
    pub idx_b: usize,
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
    pub chunk_idx_a: usize,
    pub chunk_idx_b: usize,
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
    pub hash_groups: Vec<(u64, Vec<usize>)>,
}

pub fn collect_filtered_indices(
    chunks: &[ParsedChunk],
    exclude_tests: bool,
    min_lines: usize,
) -> Vec<usize> {
    chunks.iter().enumerate().filter(|&(_, c)| {
        if exclude_tests && is_test_chunk(c) { return false; }
        if min_lines > 0 && chunk_lines(c) < min_lines { return false; }
        true
    }).map(|(i, _)| i).collect()
}

pub fn find_hash_groups(
    chunks: &[ParsedChunk],
    candidate_indices: &[usize],
    k: usize,
) -> Vec<(u64, Vec<usize>)> {
    let mut hash_groups = collect_ast_hash_groups(chunks, candidate_indices);
    for indices in hash_groups.values_mut().filter(|v| v.len() > 1) {
        remove_overlapping_chunks(chunks, indices);
    }
    let mut groups: Vec<(u64, Vec<usize>)> = hash_groups.into_iter()
        .filter(|(_, v)| v.len() > 1).collect();
    groups.sort_unstable_by(|a, b| b.1.len().cmp(&a.1.len()));
    groups.truncate(k);
    groups
}

pub fn find_minhash_pairs(
    chunks: &[ParsedChunk],
    candidate_indices: &[usize],
    threshold: f32,
    k: usize,
) -> Vec<DupePair> {
    let entries = collect_minhash_entries(chunks, candidate_indices);
    if entries.len() < 2 { return Vec::new(); }
    let mut pairs = minhash_all_pairs(&entries, f64::from(threshold));
    finalize_pairs(&mut pairs, chunks, k);
    pairs
}

pub fn run_unified_pipeline(
    chunks: &[ParsedChunk],
    candidate_indices: &[usize],
    threshold: f32,
    k: usize,
    stages: &RunStages,
    min_sub_lines: usize,
) -> Result<(Vec<UnifiedDupePair>, Vec<SubBlockClone>)> {
    let mut candidates: HashSet<(usize, usize)> = HashSet::new();

    if stages.ast {
        stage1_ast_hash(chunks, candidate_indices, &mut candidates);
    }
    if stages.minhash {
        stage1_minhash(chunks, candidate_indices, &mut candidates);
    }

    eprintln!("Stage 1: {} candidate pairs", candidates.len());

    if candidates.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let minhash_map: HashMap<usize, Vec<u64>> = candidate_indices.iter()
        .filter_map(|&idx| get_minhash(&chunks[idx]).map(|sig| (idx, sig))).collect();
    let ast_map: HashMap<usize, u64> = candidate_indices.iter()
        .filter_map(|&idx| compute_ast_hash(&chunks[idx]).map(|h| (idx, h))).collect();

    let mut pairs: Vec<UnifiedDupePair> = candidates.into_iter().filter_map(|(idx_a, idx_b)| {
        if chunks_overlap(&chunks[idx_a], &chunks[idx_b]) { return None; }
        #[expect(clippy::cast_possible_truncation)]
        let jaccard = match (minhash_map.get(&idx_a), minhash_map.get(&idx_b)) {
            (Some(sa), Some(sb)) => minhash::jaccard_from_minhash(sa, sb) as f32,
            _ => 0.0,
        };
        let ast_match = matches!((ast_map.get(&idx_a), ast_map.get(&idx_b)), (Some(ha), Some(hb)) if ha == hb);
        let score = if ast_match { 1.0_f32.max(jaccard) } else { jaccard };
        (score >= threshold).then_some(UnifiedDupePair { idx_a, idx_b, score, jaccard, ast_match })
    }).collect();

    pairs.sort_unstable_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    pairs.truncate(k);

    let sub_clones = find_sub_block_clones(chunks, candidate_indices, min_sub_lines);

    Ok((pairs, sub_clones))
}

fn canonical_pair(a: usize, b: usize) -> (usize, usize) {
    if a < b { (a, b) } else { (b, a) }
}

fn stage1_ast_hash(
    chunks: &[ParsedChunk],
    candidate_indices: &[usize],
    candidates: &mut HashSet<(usize, usize)>,
) {
    let hash_groups = collect_ast_hash_groups(chunks, candidate_indices);
    let mut ast_pairs = 0usize;
    for indices in hash_groups.values().filter(|v| v.len() > 1) {
        for i in 0..indices.len() {
            for j in (i + 1)..indices.len() {
                candidates.insert(canonical_pair(indices[i], indices[j]));
                ast_pairs += 1;
            }
        }
    }
    eprintln!("  AST hash: {ast_pairs} candidate pairs");
}

fn stage1_minhash(
    chunks: &[ParsedChunk],
    candidate_indices: &[usize],
    candidates: &mut HashSet<(usize, usize)>,
) {
    let entries = collect_minhash_entries(chunks, candidate_indices);
    if entries.len() < 2 {
        eprintln!("  MinHash: not enough chunks ({})", entries.len());
        return;
    }
    let pairs = minhash_all_pairs(&entries, 0.3);
    eprintln!("  MinHash: {} candidate pairs (threshold=0.30)", pairs.len());
    candidates.extend(pairs.into_iter().map(|p| canonical_pair(p.idx_a, p.idx_b)));
}

fn find_sub_block_clones(
    chunks: &[ParsedChunk],
    candidate_indices: &[usize],
    min_sub_lines: usize,
) -> Vec<SubBlockClone> {
    // sub_block_hashes is not computed currently — return empty
    // This field would need to be added to ParsedChunk if needed in the future.
    let _ = (chunks, candidate_indices, min_sub_lines);
    Vec::new()
}

fn is_test_chunk(c: &ParsedChunk) -> bool {
    if crate::graph::is_test_path(&c.file) { return true; }
    if c.name.contains("test_") { return true; }
    c.is_test
}

fn chunk_source_range(c: &ParsedChunk) -> Option<(&str, usize, usize)> {
    let (s, e) = c.lines?;
    Some((c.file.as_str(), s, e))
}

pub fn chunks_overlap(a: &ParsedChunk, b: &ParsedChunk) -> bool {
    let (Some((fa, sa, ea)), Some((fb, sb, eb))) =
        (chunk_source_range(a), chunk_source_range(b)) else { return false };
    fa == fb && sa <= eb && sb <= ea
}

pub fn chunk_lines(c: &ParsedChunk) -> usize {
    let Some((s, e)) = c.lines else { return 0 };
    e.saturating_sub(s) + 1
}

fn compute_ast_hash(c: &ParsedChunk) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    // Reproduce the AST hash: hash normalized signature + calls
    let sig = c.signature.as_deref().unwrap_or("");
    if sig.is_empty() && c.calls.is_empty() { return None; }
    let mut hasher = std::hash::DefaultHasher::new();
    sig.hash(&mut hasher);
    for call in &c.calls { call.hash(&mut hasher); }
    Some(hasher.finish())
}

fn get_minhash(c: &ParsedChunk) -> Option<Vec<u64>> {
    c.minhash.as_ref().and_then(|hex| minhash::minhash_from_hex(hex))
}

fn collect_ast_hash_groups(
    chunks: &[ParsedChunk],
    candidate_indices: &[usize],
) -> HashMap<u64, Vec<usize>> {
    let mut groups: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut missing = 0u32;
    for &idx in candidate_indices {
        match compute_ast_hash(&chunks[idx]) {
            Some(k) => groups.entry(k).or_default().push(idx),
            None => missing += 1,
        }
    }
    if missing > 0 { eprintln!("  ast_hash: {missing} chunks without ast_hash"); }
    groups
}

fn collect_minhash_entries(
    chunks: &[ParsedChunk],
    candidate_indices: &[usize],
) -> Vec<(usize, Vec<u64>)> {
    let entries: Vec<_> = candidate_indices.iter()
        .filter_map(|&idx| get_minhash(&chunks[idx]).map(|sig| (idx, sig)))
        .collect();
    let missing = candidate_indices.len() - entries.len();
    if missing > 0 { eprintln!("  MinHash: {missing} chunks without minhash"); }
    entries
}

fn minhash_all_pairs(entries: &[(usize, Vec<u64>)], threshold: f64) -> Vec<DupePair> {
    let n = entries.len();
    (0..n)
        .into_par_iter()
        .flat_map_iter(|i| {
            let (idx_a, sig_a) = &entries[i];
            ((i + 1)..n).filter_map(move |j| {
                let (idx_b, sig_b) = &entries[j];
                let sim = minhash::jaccard_from_minhash(sig_a, sig_b);
                #[expect(clippy::cast_possible_truncation)]
                (sim >= threshold).then_some(DupePair {
                    idx_a: *idx_a,
                    idx_b: *idx_b,
                    similarity: sim as f32,
                })
            })
        })
        .collect()
}

fn finalize_pairs(pairs: &mut Vec<DupePair>, chunks: &[ParsedChunk], k: usize) {
    pairs.retain(|p| !chunks_overlap(&chunks[p.idx_a], &chunks[p.idx_b]));
    pairs.sort_unstable_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
    pairs.truncate(k);
}

fn remove_overlapping_chunks(chunks: &[ParsedChunk], indices: &mut Vec<usize>) {
    let mut i = 0;
    while i < indices.len() {
        let mut j = i + 1;
        while j < indices.len() {
            if chunks_overlap(&chunks[indices[i]], &chunks[indices[j]]) {
                if chunk_lines(&chunks[indices[i]]) >= chunk_lines(&chunks[indices[j]]) {
                    indices.swap_remove(j);
                } else {
                    indices.swap_remove(i);
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

#[cfg(test)]
pub(crate) fn ranges_overlap(s1: usize, e1: usize, s2: usize, e2: usize) -> bool {
    s1 <= e2 && s2 <= e1
}
