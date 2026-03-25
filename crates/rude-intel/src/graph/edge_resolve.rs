//! Edge resolution strategies for call graph construction.
//!
//! Separates "how to connect edges" from the graph data structure itself.
//! Three resolvers:
//! - `resolve_by_name`: legacy name matching (exact → short fallback)
//! - `resolve_with_mir`: MIR-first, 100% accurate, name fallback for unmatched
//! - `resolve_incremental`: per-crate caching, only re-resolves changed crates

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::graph::index_tables::strip_generics_from_key;
use crate::mir_edges::MirEdgeMap;
use crate::data::parse::ParsedChunk;

// ── ChunkIndex — name-to-index lookup tables ────────────────────────

/// Bidirectional name index for resolving call names to chunk indices.
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

    fn resolve_name(&self, call: &str) -> Option<u32> {
        let lower = call.to_lowercase();
        self.exact.get(&lower).copied()
            .or_else(|| {
                let short = lower.rsplit("::").next().unwrap_or(&lower);
                self.short.get(short).copied()
            })
    }
}

// ── Resolved edges output ───────────────────────────────────────────

/// Accumulated adjacency state from edge resolution.
pub(crate) struct ResolvedEdges {
    pub callees: Vec<Vec<u32>>,
    pub callers: Vec<Vec<u32>>,
    pub call_sites: Vec<Vec<(u32, u32)>>,
}

impl ResolvedEdges {
    fn new(len: usize) -> Self {
        Self {
            callees: vec![Vec::new(); len],
            callers: vec![Vec::new(); len],
            call_sites: vec![Vec::new(); len],
        }
    }

    fn add_edge(&mut self, src: usize, tgt: u32, call_line: u32) {
        let tgt_usize = tgt as usize;
        if tgt_usize != src && src < self.callees.len() && tgt_usize < self.callers.len() {
            self.callees[src].push(tgt);
            self.callers[tgt_usize].push(src as u32);
            self.call_sites[src].push((tgt, call_line));
        }
    }

    pub(crate) fn dedup(&mut self) {
        for list in &mut self.callees { list.sort_unstable(); list.dedup(); }
        for list in &mut self.callers { list.sort_unstable(); list.dedup(); }
        for sites in &mut self.call_sites { sites.sort_by_key(|&(tgt, _)| tgt); sites.dedup_by_key(|e| e.0); }
    }
}

// ── Per-crate resolved edge cache ───────────────────────────────────

/// Cached resolved edges for a single crate.
///
/// Per-crate edge cache: index-based only, validated by chunks_hash.
#[derive(bincode::Encode, bincode::Decode)]
pub(crate) struct CrateEdgeCache {
    /// Index-based edges: (src_idx, tgt_idx, call_line).
    pub idx_edges: Vec<(u32, u32, u32)>,
}

/// All per-crate caches in a single file.
/// When `chunks_hash` doesn't match current chunks, entire cache is
/// invalidated and re-resolved (chunk order changed = rare event).
#[derive(bincode::Encode, bincode::Decode)]
pub(crate) struct EdgeCacheBundle {
    /// Hash of chunk ordering at save time.
    pub chunks_hash: u64,
    /// Crate name → cache mapping.
    pub crates: Vec<(String, CrateEdgeCache)>,
}

/// Compute a fingerprint of chunk identity (file + name).
///
/// Excludes `lines` so that body-only edits (which shift line numbers)
/// don't invalidate the cache. Only function add/remove/rename changes the hash.
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

fn edge_bundle_path(db_path: &Path) -> std::path::PathBuf {
    db_path.join("cache").join("edge-cache.bin")
}

fn save_edge_bundle(db_path: &Path, bundle: &EdgeCacheBundle) -> Result<()> {
    let path = edge_bundle_path(db_path);
    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    let bytes = bincode::encode_to_vec(bundle, bincode::config::standard())
        .context("failed to encode edge cache bundle")?;
    std::fs::write(&path, bytes)
        .with_context(|| format!("failed to write edge cache: {}", path.display()))
}

/// Merge old cached crate entries with newly resolved ones, prune stale crates.
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

fn load_edge_bundle(db_path: &Path) -> Option<EdgeCacheBundle> {
    let bytes = std::fs::read(edge_bundle_path(db_path)).ok()?;
    let (bundle, _): (EdgeCacheBundle, _) =
        bincode::decode_from_slice(&bytes, bincode::config::standard()).ok()?;
    Some(bundle)
}

/// Check if a crate's edges are stale (JSONL or mir.db newer than the bundle).
fn is_crate_cache_stale(
    mir_edge_dir: &Path,
    crate_name: &str,
    bundle_mtime: Option<std::time::SystemTime>,
) -> bool {
    let Some(cache_mtime) = bundle_mtime else { return true };
    let mtime = |p: std::path::PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let jsonl = mtime(mir_edge_dir.join(format!("{crate_name}.edges.jsonl")));
    let sqlite = mtime(mir_edge_dir.join("mir.db"));
    let source = match (jsonl, sqlite) { (Some(j), Some(s)) => Some(j.max(s)), (j, s) => j.or(s) };
    // No source at all → not stale; otherwise stale if source is newer.
    source.is_some_and(|t| t > cache_mtime)
}

type MirIndexes<'a> = (
    HashMap<(&'a str, usize), u32>,
    HashMap<String, u32>,
    HashMap<String, u32>,
    HashMap<String, Vec<String>>,
);

/// Build the four lookup tables shared by `resolve_with_mir` and `resolve_incremental`.
fn build_mir_indexes(chunks: &[ParsedChunk]) -> MirIndexes<'_> {
    let mut loc_to_idx: HashMap<(&str, usize), u32> = HashMap::new();
    let mut name_to_idx: HashMap<String, u32> = HashMap::new();
    let mut suffix_to_idx: HashMap<String, u32> = HashMap::new();
    let mut file_suffix_to_normalized: HashMap<String, Vec<String>> = HashMap::new();

    for (i, c) in chunks.iter().enumerate() {
        if let Some((start, _)) = c.lines {
            loc_to_idx.insert((&c.file, start), i as u32);
        }
        let lower = strip_visibility_prefix(&c.name).to_lowercase();
        name_to_idx.insert(lower.clone(), i as u32);
        if let Some(last) = lower.rsplit("::").next() {
            suffix_to_idx.entry(last.to_owned()).or_insert(i as u32);
        }
        file_suffix_to_normalized.entry(c.file.to_lowercase()).or_default().push(c.file.clone());
    }
    for v in file_suffix_to_normalized.values_mut() { v.sort(); v.dedup(); }
    (loc_to_idx, name_to_idx, suffix_to_idx, file_suffix_to_normalized)
}

/// Resolve a single callee to a chunk index: location first, then name fallback.
fn resolve_callee(
    callee: &crate::mir_edges::CalleeInfo,
    loc_to_idx: &HashMap<(&str, usize), u32>,
    name_to_idx: &HashMap<String, u32>,
    file_suffix_to_normalized: &HashMap<String, Vec<String>>,
) -> Option<u32> {
    if !callee.file.is_empty() && callee.start_line > 0 {
        resolve_by_location(&callee.file, callee.start_line, loc_to_idx, file_suffix_to_normalized)
            .or_else(|| resolve_mir_name(&callee.name.to_lowercase(), name_to_idx))
    } else {
        resolve_mir_name(&callee.name.to_lowercase(), name_to_idx)
    }
}

/// Resolve edges for a single crate's callers and return the edge triples.
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
        let Some(callees) = mir_edges.by_caller.get(*caller_name) else { continue };
        for callee in callees {
            if let (Some(s), Some(t)) = (src, resolve_callee(callee, loc_to_idx, name_to_idx, file_suffix_to_normalized)) {
                edges.push((s, t, callee.call_line as u32));
            }
        }
    }
    edges
}

/// Incremental MIR edge resolve with per-crate caching.
///
/// Only re-resolves edges for `changed_crates` (or stale crates).
/// Caches store edges as `(src_key, tgt_key, call_line)` using stable
/// identity keys (file:line), so they survive chunk reordering/add/remove.
pub(crate) fn resolve_incremental(
    chunks: &[ParsedChunk],
    index: &ChunkIndex,
    mir_edges: &MirEdgeMap,
    changed_crates: &[String],
    db_path: &Path,
    mir_edge_dir: &Path,
) -> ResolvedEdges {
    let mut adj = ResolvedEdges::new(chunks.len());
    let mut mir_resolved: usize = 0;
    let mut cache_loaded: usize = 0;
    let mut re_resolved_crates: usize = 0;

    let (loc_to_idx, name_to_idx, suffix_to_idx, file_suffix_to_normalized) =
        build_mir_indexes(chunks);

    let chunks_hash = compute_chunks_hash(chunks);

    // Load entire bundle in one I/O operation
    let bundle = load_edge_bundle(db_path);
    let bundle_mtime = std::fs::metadata(edge_bundle_path(db_path))
        .and_then(|m| m.modified()).ok();
    let hash_matches = bundle.as_ref().is_some_and(|b| b.chunks_hash == chunks_hash);

    // If hash doesn't match (chunk order changed), discard entire cache.
    // This is rare (only on file add/remove) and ensures correctness.
    let cached: HashMap<&str, &CrateEdgeCache> = if hash_matches {
        bundle.as_ref()
            .map(|b| b.crates.iter().map(|(name, cache)| (name.as_str(), cache)).collect())
            .unwrap_or_default()
    } else {
        HashMap::new()
    };

    let all_crate_names = mir_edges.crate_names();
    let changed_set: std::collections::HashSet<&str> =
        changed_crates.iter().map(String::as_str).collect();

    let mut new_crates: HashMap<String, CrateEdgeCache> = HashMap::new();

    for crate_name in &all_crate_names {
        let needs_resolve = changed_set.contains(crate_name)
            || !hash_matches
            || is_crate_cache_stale(mir_edge_dir, crate_name, bundle_mtime);

        if !needs_resolve {
            if let Some(cache) = cached.get(crate_name) {
                cache_loaded += cache.idx_edges.len();
                for &(s, t, line) in &cache.idx_edges {
                    adj.add_edge(s as usize, t, line);
                }
                continue;
            }
        }
        // Re-resolve: either forced by change/staleness or no cache available.
        let mut idx_edges = resolve_crate_edges(crate_name, mir_edges, &loc_to_idx, &name_to_idx, &suffix_to_idx, &file_suffix_to_normalized);
        idx_edges.sort_unstable();
        idx_edges.dedup();
        re_resolved_crates += 1;
        mir_resolved += idx_edges.len();
        for &(s, t, line) in &idx_edges {
            adj.add_edge(s as usize, t, line);
        }
        new_crates.insert(crate_name.to_string(), CrateEdgeCache { idx_edges });
    }

    // Merge cached + newly resolved, prune stale crates, save.
    let final_crates = merge_crate_caches(bundle, hash_matches, new_crates, &all_crate_names);
    let new_bundle = EdgeCacheBundle { chunks_hash, crates: final_crates };
    let _ = save_edge_bundle(db_path, &new_bundle);

    // Type ref edges from chunks (always re-resolved, cheap)
    for (src, chunk) in chunks.iter().enumerate() {
        resolve_type_refs(src, chunk, index, &mut adj);
    }

    adj.dedup();
    eprintln!("      [edge-resolve] incremental: resolved={mir_resolved} cached={cache_loaded} re-resolved_crates={re_resolved_crates}/{}", all_crate_names.len());
    adj
}

// ── Name-based resolver (legacy) ────────────────────────────────────

/// Resolve call edges by name matching only.
pub(crate) fn resolve_by_name(chunks: &[ParsedChunk], index: &ChunkIndex) -> ResolvedEdges {
    let mut adj = ResolvedEdges::new(chunks.len());

    for (src, chunk) in chunks.iter().enumerate() {
        for (call_idx, call) in chunk.calls.iter().enumerate() {
            if let Some(tgt) = index.resolve_name(call) {
                let line = chunk.call_lines.get(call_idx).copied().unwrap_or(0);
                adj.add_edge(src, tgt, line);
            }
        }
        resolve_type_refs(src, chunk, index, &mut adj);
    }

    adj.dedup();
    adj
}

// ── MIR-based resolver ──────────────────────────────────────────────

/// Resolve call edges directly from MIR edge map.
///
/// Iterates MIR caller→callee pairs and maps them to chunk indices.
/// Does not depend on chunk.calls (which may be empty in MIR mode).
pub(crate) fn resolve_with_mir(
    chunks: &[ParsedChunk],
    index: &ChunkIndex,
    mir_edges: &MirEdgeMap,
) -> ResolvedEdges {
    let mut adj = ResolvedEdges::new(chunks.len());

    let (loc_to_idx, name_to_idx, suffix_to_idx, file_suffix_to_normalized) =
        build_mir_indexes(chunks);

    for (caller_name, callees) in &mir_edges.by_caller {
        let src = resolve_by_loc_or_name(caller_name, &name_to_idx, &suffix_to_idx);
        for callee in callees {
            if let (Some(s), Some(t)) = (src, resolve_callee(callee, &loc_to_idx, &name_to_idx, &file_suffix_to_normalized)) {
                adj.add_edge(s as usize, t, callee.call_line as u32);
            }
        }
    }

    // Type ref edges from chunks
    for (src, chunk) in chunks.iter().enumerate() {
        resolve_type_refs(src, chunk, index, &mut adj);
    }

    adj.dedup();
    adj
}

/// Resolve by (file, start_line) with ±1 line tolerance.
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

/// Resolve a caller by name, with closure-suffix stripping and suffix fallback.
fn resolve_by_loc_or_name(
    caller_name: &str,
    name_to_idx: &HashMap<String, u32>,
    suffix_to_idx: &HashMap<String, u32>,
) -> Option<u32> {
    let lower = caller_name.to_lowercase();
    resolve_mir_name(&lower, name_to_idx)
        .or_else(|| suffix_to_idx.get(strip_closure_suffix(&lower)).copied())
}

/// Name-based fallback: exact match, then strip crate prefix.
fn resolve_mir_name(name: &str, name_to_idx: &HashMap<String, u32>) -> Option<u32> {
    let name = strip_closure_suffix(name);
    name_to_idx.get(name).copied().or_else(|| {
        let (_, rest) = name.split_once("::")?;
        name_to_idx.get(rest).copied()
    })
}

/// Strip `pub(...)` / `pub` visibility prefix from chunk names.
fn strip_visibility_prefix(name: &str) -> &str {
    if let Some(rest) = name.strip_prefix("pub(") {
        if let Some(close) = rest.find(") ") { return &rest[close + 2..]; }
    }
    name.strip_prefix("pub ").unwrap_or(name)
}

/// Strip `{closure#N}` suffixes: `daemon::run::{closure#0}` → `daemon::run`.
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


