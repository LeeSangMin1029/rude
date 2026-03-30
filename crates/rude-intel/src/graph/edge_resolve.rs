use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::graph::index_tables::strip_generics_from_key;
use crate::mir_edges::MirEdgeMap;
use crate::data::parse::ParsedChunk;

pub(crate) struct ChunkIndex {
    pub exact: HashMap<String, u32>,
    pub short: HashMap<String, u32>,
}

impl ChunkIndex {
    pub fn build(chunks: &[ParsedChunk]) -> Self {
        let mut exact = HashMap::new();
        let mut short = HashMap::new();

        for (i, c) in chunks.iter().enumerate() {
            let idx = i as u32;
            let lower = c.name.to_lowercase();

            exact.insert(lower.clone(), idx);
            let stripped = strip_generics_from_key(&lower);
            if stripped != lower { exact.entry(stripped).or_insert(idx); }

            if let Some(s) = c.name.rsplit("::").next() {
                short.entry(s.to_lowercase()).or_insert(idx);
            }

            if let Some((prefix, method_name)) = lower.rsplit_once("::") {
                if let Some(owner_leaf) = prefix.rsplit_once("::").map(|p| p.1) {
                    let alias = format!("{owner_leaf}::{method_name}");
                    if alias != lower { exact.entry(alias).or_insert(idx); }
                }
                if let Some(for_pos) = prefix.find(" for ") {
                    let concrete = &prefix[for_pos + 5..];
                    let leaf = concrete.rsplit("::").next().unwrap_or(concrete)
                        .split('<').next().unwrap_or("");
                    if !leaf.is_empty() {
                        exact.entry(format!("{leaf}::{method_name}")).or_insert(idx);
                    }
                }
            }
        }

        Self { exact, short }
    }

}

pub(crate) struct ResolvedEdges {
    pub callees: Vec<Vec<u32>>,
    pub callers: Vec<Vec<u32>>,
    pub call_sites: Vec<Vec<(u32, u32)>>,
}

impl ResolvedEdges {
    fn new(len: usize) -> Self {
        Self { callees: vec![vec![]; len], callers: vec![vec![]; len], call_sites: vec![vec![]; len] }
    }

    fn add_edge(&mut self, src: usize, tgt: u32, call_line: u32) {
        let t = tgt as usize;
        if t != src && src < self.callees.len() && t < self.callers.len() {
            self.callees[src].push(tgt);
            self.callers[t].push(src as u32);
            self.call_sites[src].push((tgt, call_line));
        }
    }

    pub(crate) fn empty(len: usize) -> Self { Self::new(len) }

    pub(crate) fn dedup(&mut self) {
        for v in &mut self.callees { v.sort_unstable(); v.dedup(); }
        for v in &mut self.callers { v.sort_unstable(); v.dedup(); }
        for v in &mut self.call_sites { v.sort_by_key(|&(t, _)| t); v.dedup_by_key(|e| e.0); }
    }
}
#[derive(bincode::Encode, bincode::Decode)]
pub(crate) struct CrateEdgeCache {
    pub idx_edges: Vec<(u32, u32, u32)>,
}

#[derive(bincode::Encode, bincode::Decode)]
pub(crate) struct EdgeCacheBundle {
    pub chunks_hash: u64,
    pub crates: Vec<(String, CrateEdgeCache)>,
}

fn compute_chunks_hash(chunks: &[ParsedChunk]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    chunks.len().hash(&mut hasher);
    for c in chunks {
        c.file.hash(&mut hasher);
        c.name.hash(&mut hasher);
    }
    hasher.finish()
}

fn save_edge_bundle(engine: &rude_db::StorageEngine, bundle: &EdgeCacheBundle) -> Result<()> {
    let bytes = bincode::encode_to_vec(bundle, bincode::config::standard()).context("encode edge cache")?;
    engine.set_cache("edges", &bytes).context("write edge cache")
}

fn merge_crate_caches(
    bundle: Option<EdgeCacheBundle>,
    hash_matches: bool,
    new_crates: HashMap<String, CrateEdgeCache>,
    all_crate_names: &std::collections::HashSet<&str>,
) -> Vec<(String, CrateEdgeCache)> {
    let mut final_crates: Vec<(String, CrateEdgeCache)> = if hash_matches {
        bundle.map(|b| b.crates).unwrap_or_default()
            .into_iter()
            .filter(|(name, _)| !new_crates.contains_key(name) && all_crate_names.contains(name.as_str()))
            .collect()
    } else {
        Vec::new()
    };
    final_crates.extend(new_crates);
    let pre = final_crates.len();
    final_crates.retain(|(name, _)| all_crate_names.contains(name.as_str()));
    let pruned = pre - final_crates.len();
    if pruned > 0 { eprintln!("      [edge-resolve] pruned {pruned} stale crate(s) from edge cache"); }
    final_crates
}

fn load_edge_bundle(engine: &rude_db::StorageEngine) -> Option<EdgeCacheBundle> {
    let bytes = engine.get_cache("edges").ok()??;
    bincode::decode_from_slice::<EdgeCacheBundle, _>(&bytes, bincode::config::standard()).ok().map(|(b, _)| b)
}

fn is_crate_cache_stale(
    mir_edge_dir: &Path,
    bundle_mtime: Option<std::time::SystemTime>,
) -> bool {
    let Some(cache_mtime) = bundle_mtime else { return true };
    std::fs::metadata(mir_edge_dir.join("mir.db"))
        .and_then(|m| m.modified())
        .is_ok_and(|t| t > cache_mtime)
}

type MirIndexes<'a> = (
    HashMap<(&'a str, usize), u32>,
    HashMap<String, u32>,
    HashMap<String, Vec<u32>>,
    HashMap<String, u32>,
    HashMap<String, Vec<String>>,
);

fn build_mir_indexes(chunks: &[ParsedChunk]) -> MirIndexes<'_> {
    let mut loc_to_idx: HashMap<(&str, usize), u32> = HashMap::new();
    let mut name_to_idx: HashMap<String, u32> = HashMap::new();
    let mut name_to_all: HashMap<String, Vec<u32>> = HashMap::new();
    let mut suffix_to_idx: HashMap<String, u32> = HashMap::new();
    let mut file_suffix_to_normalized: HashMap<String, Vec<String>> = HashMap::new();

    for (i, c) in chunks.iter().enumerate() {
        let is_synthetic = c.lines == Some((0, 0));
        if let Some((start, _)) = c.lines {
            if !is_synthetic { loc_to_idx.insert((&c.file, start), i as u32); }
        }
        let lower = strip_visibility_prefix(&c.name).to_lowercase();
        name_to_all.entry(lower.clone()).or_default().push(i as u32);
        if !is_synthetic || !name_to_idx.contains_key(&lower) {
            name_to_idx.insert(lower.clone(), i as u32);
        }
        if let Some(last) = lower.rsplit("::").next() {
            if !is_synthetic { suffix_to_idx.entry(last.to_owned()).or_insert(i as u32); }
        }
        file_suffix_to_normalized.entry(c.file.to_lowercase()).or_default().push(c.file.clone());
    }
    for v in file_suffix_to_normalized.values_mut() { v.sort(); v.dedup(); }
    (loc_to_idx, name_to_idx, name_to_all, suffix_to_idx, file_suffix_to_normalized)
}

fn resolve_callee(
    callee: &crate::mir_edges::CalleeInfo,
    loc_to_idx: &HashMap<(&str, usize), u32>,
    name_to_idx: &HashMap<String, u32>,
    file_suffix_to_normalized: &HashMap<String, Vec<String>>,
) -> Option<u32> {
    let by_name = || resolve_mir_name(&callee.name.to_lowercase(), name_to_idx);
    if !callee.file.is_empty() && callee.start_line > 0 {
        resolve_by_location(&callee.file, callee.start_line, loc_to_idx, file_suffix_to_normalized)
            .or_else(by_name)
    } else {
        by_name()
    }
}

fn resolve_trait_method_impls(
    callee_name: &str,
    name_to_idx: &HashMap<String, u32>,
) -> Vec<u32> {
    let lower = callee_name.to_lowercase();
    let lower = strip_closure_suffix(&lower);
    let (trait_part, method) = match lower.rsplit_once("::") {
        Some(pair) => pair,
        None => return Vec::new(),
    };
    let trait_leaf = trait_part.rsplit("::").next().unwrap_or(trait_part);
    let suffix = format!("{trait_leaf}>::{method}");
    name_to_idx.iter()
        .filter(|(k, _)| k.contains(" as ") && k.ends_with(&suffix))
        .map(|(_, &idx)| idx)
        .collect()
}

fn resolve_crate_edges(
    crate_name: &str,
    mir_edges: &MirEdgeMap,
    loc_to_idx: &HashMap<(&str, usize), u32>,
    name_to_idx: &HashMap<String, u32>,
    suffix_to_idx: &HashMap<String, u32>,
    file_suffix_to_normalized: &HashMap<String, Vec<String>>,
) -> Vec<(u32, u32, u32)> {
    let callers = mir_edges.callers_for_crate(crate_name);
    let mut edges = Vec::new();
    for caller_name in &callers {
        let src = resolve_by_loc_or_name(caller_name, name_to_idx, suffix_to_idx);
        let Some(s) = src else { continue };
        let Some(callees) = mir_edges.by_caller.get(*caller_name) else { continue };
        for callee in callees {
            let targets = match resolve_callee(callee, loc_to_idx, name_to_idx, file_suffix_to_normalized) {
                Some(t) => vec![t],
                None => resolve_trait_method_impls(&callee.name, name_to_idx),
            };
            for t in targets {
                edges.push((s, t, callee.call_line as u32));
            }
        }
    }
    edges
}

pub(crate) fn resolve_incremental(
    chunks: &[ParsedChunk],
    index: &ChunkIndex,
    mir_edges: Option<&MirEdgeMap>,
    changed_crates: &[String],
    db_path: &Path,
    mir_edge_dir: &Path,
) -> ResolvedEdges {
    let mut adj = ResolvedEdges::new(chunks.len());
    let mut mir_resolved: usize = 0;
    let mut cache_loaded: usize = 0;
    let mut re_resolved_crates: usize = 0;

    let (loc_to_idx, name_to_idx, _name_to_all, suffix_to_idx, file_suffix_to_normalized) =
        build_mir_indexes(chunks);

    let chunks_hash = compute_chunks_hash(chunks);
    let edge_engine = rude_db::StorageEngine::open(db_path).ok();
    let bundle = edge_engine.as_ref().and_then(load_edge_bundle);
    let bundle_mtime = std::fs::metadata(db_path.join("store.db")).and_then(|m| m.modified()).ok();
    let hash_matches = bundle.as_ref().is_some_and(|b| b.chunks_hash == chunks_hash);
    let cached: HashMap<&str, &CrateEdgeCache> = if hash_matches {
        bundle.as_ref().map(|b| b.crates.iter().map(|(n, c)| (n.as_str(), c)).collect()).unwrap_or_default()
    } else { HashMap::new() };

    // Get crate list from MirEdgeMap if available, otherwise from edge cache + changed_crates
    let all_crate_names: std::collections::HashSet<String> = if let Some(me) = mir_edges {
        me.crate_names().into_iter().map(|s| s.to_owned()).collect()
    } else {
        let mut names: std::collections::HashSet<String> = cached.keys().map(|s| s.to_string()).collect();
        for c in changed_crates { names.insert(c.replace('-', "_")); }
        names
    };

    // Load MirEdgeMap for changed crates only (from mir.db directly) if not provided
    let mir_db = crate::mir_edges::mir_db_path(db_path.parent().unwrap_or(db_path));
    let lazy_mir = if mir_edges.is_none() && mir_db.exists() {
        let filter: Vec<&str> = changed_crates.iter().map(|s| s.as_str()).collect();
        MirEdgeMap::from_sqlite(&mir_db, Some(&filter)).ok()
    } else { None };
    let effective_mir = mir_edges.or(lazy_mir.as_ref());

    let changed_set: std::collections::HashSet<&str> = changed_crates.iter().map(String::as_str).collect();
    let mut new_crates: HashMap<String, CrateEdgeCache> = HashMap::new();

    for crate_name in &all_crate_names {
        let needs_resolve = changed_set.contains(crate_name.as_str())
            || !hash_matches
            || is_crate_cache_stale(mir_edge_dir, bundle_mtime);

        if !needs_resolve {
            if let Some(cache) = cached.get(crate_name.as_str()) {
                cache_loaded += cache.idx_edges.len();
                for &(s, t, line) in &cache.idx_edges { adj.add_edge(s as usize, t, line); }
                continue;
            }
        }
        if let Some(me) = effective_mir {
            let mut idx_edges = resolve_crate_edges(crate_name, me, &loc_to_idx, &name_to_idx, &suffix_to_idx, &file_suffix_to_normalized);
            idx_edges.sort_unstable(); idx_edges.dedup();
            re_resolved_crates += 1;
            mir_resolved += idx_edges.len();
            for &(s, t, line) in &idx_edges { adj.add_edge(s as usize, t, line); }
            new_crates.insert(crate_name.to_string(), CrateEdgeCache { idx_edges });
        }
    }

    let all_refs: std::collections::HashSet<&str> = all_crate_names.iter().map(|s| s.as_str()).collect();
    let final_crates = merge_crate_caches(bundle, hash_matches, new_crates, &all_refs);
    if let Some(ref eng) = edge_engine {
        let _ = save_edge_bundle(eng, &EdgeCacheBundle { chunks_hash, crates: final_crates });
    }
    for (src, chunk) in chunks.iter().enumerate() { resolve_type_refs(src, chunk, index, &mut adj); }
    adj.dedup();
    eprintln!("      [edge-resolve] incremental: resolved={mir_resolved} cached={cache_loaded} re-resolved_crates={re_resolved_crates}/{}", all_crate_names.len());
    adj
}

/// Name-based resolution used only in tests (no MIR available).
pub(crate) fn resolve_by_name_test(chunks: &[ParsedChunk], index: &ChunkIndex) -> ResolvedEdges {
    fn resolve_name(index: &ChunkIndex, call: &str) -> Option<u32> {
        let lower = call.to_lowercase();
        let short = lower.rsplit("::").next().unwrap_or(&lower);
        index.exact.get(&lower).copied().or_else(|| index.short.get(short).copied())
    }
    let mut adj = ResolvedEdges::new(chunks.len());
    for (src, chunk) in chunks.iter().enumerate() {
        for (call_idx, call) in chunk.calls.iter().enumerate() {
            if let Some(tgt) = resolve_name(index, call) {
                adj.add_edge(src, tgt, chunk.call_lines.get(call_idx).copied().unwrap_or(0));
            }
        }
        resolve_type_refs(src, chunk, index, &mut adj);
    }
    adj.dedup();
    adj
}

pub(crate) fn resolve_with_mir(
    chunks: &[ParsedChunk],
    index: &ChunkIndex,
    mir_edges: &MirEdgeMap,
) -> ResolvedEdges {
    let mut adj = ResolvedEdges::new(chunks.len());

    let (loc_to_idx, name_to_idx, name_to_all, suffix_to_idx, file_suffix_to_normalized) =
        build_mir_indexes(chunks);

    for (caller_name, callees) in &mir_edges.by_caller {
        let caller_files = mir_edges.caller_files.get(caller_name.as_str());
        let srcs = resolve_all_callers(
            caller_name, caller_files,
            chunks, &name_to_idx, &name_to_all, &suffix_to_idx,
        );
        for callee in callees {
            let targets = match resolve_callee(callee, &loc_to_idx, &name_to_idx, &file_suffix_to_normalized) {
                Some(t) => vec![t],
                None => resolve_trait_method_impls(&callee.name, &name_to_idx),
            };
            for t in targets {
                let tgt_root = file_root(&chunks[t as usize].file);
                if let Some(&s) = srcs.iter().find(|&&s| file_root(&chunks[s as usize].file) == tgt_root) {
                    adj.add_edge(s as usize, t, callee.call_line as u32);
                }
            }
        }
    }

    for (src, chunk) in chunks.iter().enumerate() { resolve_type_refs(src, chunk, index, &mut adj); }
    adj.dedup();
    adj
}


fn resolve_all_callers(
    caller_name: &str, caller_files: Option<&Vec<String>>,
    chunks: &[ParsedChunk],
    name_to_idx: &HashMap<String, u32>,
    name_to_all: &HashMap<String, Vec<u32>>,
    suffix_to_idx: &HashMap<String, u32>,
) -> Vec<u32> {
    let lower = caller_name.to_lowercase();
    let candidates = name_to_all.get(&lower)
        .or_else(|| lower.split_once("::").and_then(|(_, r)| name_to_all.get(r)));
    if let Some(cands) = candidates {
        if cands.len() > 1 {
            let real: Vec<u32> = cands.iter().copied()
                .filter(|&idx| chunks[idx as usize].lines != Some((0, 0)))
                .collect();
            // If caller_files known, narrow to matching files
            if let Some(files) = caller_files {
                let matched: Vec<u32> = real.iter().copied()
                    .filter(|&idx| files.iter().any(|f| chunks[idx as usize].file == *f))
                    .collect();
                if !matched.is_empty() { return matched; }
            }
            if !real.is_empty() { return real; }
        }
    }
    resolve_by_loc_or_name(caller_name, name_to_idx, suffix_to_idx)
        .into_iter().collect()
}

fn file_root(file: &str) -> &str {
    // "crates/rude/src/main.rs" → "crates/"
    // "src/daemon.rs" → "src/"
    // Workspace membership: files starting with same root belong together
    if let Some(pos) = file.find('/') { &file[..=pos] } else { file }
}

fn resolve_by_location(
    file: &str,
    start_line: usize,
    loc_to_idx: &HashMap<(&str, usize), u32>,
    file_suffix_to_normalized: &HashMap<String, Vec<String>>,
) -> Option<u32> {
    let file_lower = file.to_lowercase();
    let candidates: Vec<&str> = file_suffix_to_normalized.get(&file_lower)
        .map(|v| v.iter().map(String::as_str).collect())
        .unwrap_or_else(|| {
            file_suffix_to_normalized.iter()
                .filter(|(_, norms)| norms.iter().any(|n| n.ends_with(file) || file.ends_with(n.as_str())))
                .flat_map(|(_, norms)| norms.iter().map(String::as_str))
                .collect()
        });
    for norm_file in &candidates {
        for delta in [0isize, 1, -1] {
            let line = (start_line as isize + delta) as usize;
            if let Some(&idx) = loc_to_idx.get(&(*norm_file, line)) { return Some(idx); }
        }
    }
    None
}

fn resolve_by_loc_or_name(caller_name: &str, name_to_idx: &HashMap<String, u32>, suffix_to_idx: &HashMap<String, u32>) -> Option<u32> {
    let lower = caller_name.to_lowercase();
    resolve_mir_name(&lower, name_to_idx).or_else(|| suffix_to_idx.get(strip_closure_suffix(&lower)).copied())
}

fn resolve_mir_name(name: &str, name_to_idx: &HashMap<String, u32>) -> Option<u32> {
    let name = strip_closure_suffix(name);
    name_to_idx.get(name).copied().or_else(|| name.split_once("::").and_then(|(_, r)| name_to_idx.get(r).copied()))
}

fn strip_visibility_prefix(name: &str) -> &str {
    if let Some(rest) = name.strip_prefix("pub(") {
        if let Some(close) = rest.find(") ") { return &rest[close + 2..]; }
    }
    name.strip_prefix("pub ").unwrap_or(name)
}

fn strip_closure_suffix(name: &str) -> &str {
    name.find("::{closure").map_or(name, |pos| &name[..pos])
}

fn resolve_type_refs(src: usize, chunk: &ParsedChunk, index: &ChunkIndex, adj: &mut ResolvedEdges) {
    for ty in &chunk.types {
        let lower = ty.to_lowercase();
        if let Some(&tgt) = index.exact.get(&lower).or_else(|| index.short.get(&lower)) {
            if tgt as usize != src {
                adj.callees[src].push(tgt);
                adj.callers[tgt as usize].push(src as u32);
            }
        }
    }
}
