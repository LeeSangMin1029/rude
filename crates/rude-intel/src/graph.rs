//! Call graph adjacency list built from `ParsedChunk` data.
//!
//! Provides `CallGraph` — a pre-built, bincode-cached graph that maps
//! chunk indices to their callees and callers for fast BFS traversal.
//!
//! Edge resolution (how calls are connected) is handled by [`crate::edge_resolve`].

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::edge_resolve::{self, ChunkIndex, ResolvedEdges};
use crate::index_tables;
use crate::mir_edges::MirEdgeMap;
use crate::parse::ParsedChunk;

/// Source hash — auto-computed by build.rs for cache invalidation.
const GRAPH_SOURCE_HASH: &str = env!("GRAPH_SOURCE_HASH");

// Re-export for external consumers (used by rude, stats, clones).
pub use crate::index_tables::{is_test_path, is_test_chunk};

/// Arguments for incremental edge resolution.
pub struct IncrementalArgs<'a> {
    /// Crate names that changed and need re-resolution.
    pub changed_crates: &'a [String],
    /// Directory containing per-crate `.edges.jsonl` files.
    pub mir_edge_dir: &'a Path,
}

// ── CallGraph struct ────────────────────────────────────────────────

/// Pre-built call graph with bidirectional adjacency lists.
#[derive(bincode::Encode, bincode::Decode)]
pub struct CallGraph {
    version: String,
    pub names: Vec<String>,
    pub files: Vec<String>,
    pub kinds: Vec<String>,
    pub lines: Vec<Option<(usize, usize)>>,
    pub signatures: Vec<Option<String>>,
    pub name_index: Vec<(String, u32)>,
    pub callees: Vec<Vec<u32>>,
    pub callers: Vec<Vec<u32>>,
    pub is_test: Vec<bool>,
    pub trait_impls: Vec<Vec<u32>>,
    /// Reverse of `trait_impls`: impl_idx → trait_idx (None if not a trait impl).
    pub impl_of_trait: Vec<Option<u32>>,
    /// Per-function: parent trait impl block index (None if not in a trait impl).
    pub fn_trait_impl: Vec<Option<u32>>,
    pub call_sites: Vec<Vec<(u32, u32)>>,
    pub field_access_index: Vec<(String, Vec<u32>)>,
}

// ── Build ───────────────────────────────────────────────────────────

impl CallGraph {
    /// Build call graph using name matching (legacy).
    pub fn build(chunks: &[ParsedChunk]) -> Self {
        let t0 = std::time::Instant::now();
        let index = ChunkIndex::build(chunks);
        let adj = edge_resolve::resolve_by_name(chunks, &index);
        eprintln!("      [graph] name-resolve: {:.1}ms ({} chunks)", t0.elapsed().as_secs_f64() * 1000.0, chunks.len());
        Self::assemble(chunks, &index, adj)
    }

    /// Build call graph using MIR-resolved edges (100% accurate).
    /// Falls back to name matching for edges not found in MIR.
    pub fn build_with_mir(chunks: &[ParsedChunk], mir_edges: &MirEdgeMap) -> Self {
        let t0 = std::time::Instant::now();
        let index = ChunkIndex::build(chunks);
        let adj = edge_resolve::resolve_with_mir(chunks, &index, mir_edges);
        eprintln!("      [graph] mir-resolve: {:.1}ms ({} chunks)", t0.elapsed().as_secs_f64() * 1000.0, chunks.len());
        Self::assemble(chunks, &index, adj)
    }

    /// Build call graph with incremental MIR edge resolve.
    ///
    /// Only re-resolves edges for `changed_crates`; loads cached results
    /// for unchanged crates. Falls back to full resolve if
    /// `changed_crates` is empty.
    pub fn build_with_mir_incremental(
        chunks: &[ParsedChunk],
        mir_edges: &MirEdgeMap,
        changed_crates: &[String],
        db_path: &Path,
        mir_edge_dir: &Path,
    ) -> Self {
        let t0 = std::time::Instant::now();
        let index = ChunkIndex::build(chunks);
        let adj = edge_resolve::resolve_incremental(
            chunks, &index, mir_edges, changed_crates, db_path, mir_edge_dir,
        );
        eprintln!("      [graph] mir-incremental: {:.1}ms ({} chunks)", t0.elapsed().as_secs_f64() * 1000.0, chunks.len());
        Self::assemble(chunks, &index, adj)
    }

    /// Assemble CallGraph from resolved edges + chunk metadata.
    fn assemble(chunks: &[ParsedChunk], index: &ChunkIndex, adj: ResolvedEdges, ) -> Self {
        let t = std::time::Instant::now();
        let owner_field_types = index_tables::collect_owner_field_types(chunks);

        let len = chunks.len();
        let mut names = Vec::with_capacity(len);
        let mut files = Vec::with_capacity(len);
        let mut kinds = Vec::with_capacity(len);
        let mut lines_vec = Vec::with_capacity(len);
        let mut signatures = Vec::with_capacity(len);
        let mut is_test = Vec::with_capacity(len);
        let mut name_index = Vec::with_capacity(len);

        for (i, c) in chunks.iter().enumerate() {
            name_index.push((c.name.to_lowercase(), i as u32));
            names.push(c.name.clone());
            files.push(c.file.clone());
            kinds.push(c.kind.clone());
            lines_vec.push(c.lines);
            signatures.push(c.signature.clone());
            is_test.push(is_test_chunk(c));
        }
        name_index.sort_by(|a, b| a.0.cmp(&b.0));

        let (trait_impls, field_access_index) = rayon::join(
            || index_tables::build_trait_impls(&names, &kinds, &index.exact, &index.short),
            || index_tables::build_field_access_index(chunks, &owner_field_types),
        );
        let mut impl_of_trait: Vec<Option<u32>> = vec![None; len];
        for (trait_idx, impls) in trait_impls.iter().enumerate() {
            for &impl_idx in impls {
                impl_of_trait[impl_idx as usize] = Some(trait_idx as u32);
            }
        }
        let fn_trait_impl = index_tables::build_fn_trait_impl(&names, &kinds);

        eprintln!("      [graph] assemble: {:.1}ms", t.elapsed().as_secs_f64() * 1000.0);

        Self {
            version: GRAPH_SOURCE_HASH.to_owned(),
            names, files, kinds,
            lines: lines_vec, signatures, name_index,
            callees: adj.callees, callers: adj.callers,
            is_test, trait_impls, impl_of_trait, fn_trait_impl,
            call_sites: adj.call_sites,
            field_access_index,
        }
    }

    // ── Query API ───────────────────────────────────────────────────

    pub fn resolve(&self, name: &str) -> Vec<u32> {
        let lower = name.to_lowercase();
        let start = self.name_index.partition_point(|(n, _)| n.as_str() < lower.as_str());
        let mut results: Vec<u32> = self.name_index[start..].iter()
            .take_while(|(n, _)| n == &lower).map(|(_, idx)| *idx).collect();
        if results.is_empty() {
            let suffix = format!("::{lower}");
            results = self.name_index.iter().filter(|(n, _)| n.ends_with(&suffix)).map(|(_, idx)| *idx).collect();
        }
        results
    }

    pub fn call_site_line(&self, caller_idx: u32, callee_idx: u32) -> u32 {
        self.call_sites[caller_idx as usize].iter()
            .find(|&&(tgt, _)| tgt == callee_idx).map(|&(_, line)| line).unwrap_or(0)
    }

    pub fn find_field_access(&self, key: &str) -> Vec<u32> {
        let lower = key.to_lowercase();
        self.field_access_index.binary_search_by_key(&&*lower, |(k, _)| k.as_str())
            .ok().map(|i| self.field_access_index[i].1.clone()).unwrap_or_default()
    }

    pub fn find_field_accesses_for_type(&self, type_name: &str) -> Vec<(&str, &[u32])> {
        let prefix = format!("{}::", type_name.to_lowercase());
        let start = self.field_access_index.partition_point(|(k, _)| k.as_str() < prefix.as_str());
        self.field_access_index[start..].iter()
            .take_while(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (&k[prefix.len()..], v.as_slice())).collect()
    }

    /// Build graph from chunks + optional MIR edges, then save to disk.
    ///
    /// Shared helper used by both `prebuild_caches` (add) and
    /// `rebuild_graph_cache` (watch) to avoid duplicating the
    /// build-dispatch + save logic.
    ///
    /// When `incremental` is provided, uses per-crate edge caching for
    /// faster rebuild. Falls back to full resolve otherwise.
    pub fn rebuild(
        db: &Path,
        chunks: &[ParsedChunk],
        mir_edges: Option<&MirEdgeMap>,
        incremental: Option<IncrementalArgs<'_>>,
    ) -> Result<Self> {
        let graph = Self::build_only(chunks, mir_edges, incremental, db);
        graph.save(db)?;
        Ok(graph)
    }

    /// Build graph without saving — caller decides when/how to persist.
    pub fn build_only(
        chunks: &[ParsedChunk],
        mir_edges: Option<&MirEdgeMap>,
        incremental: Option<IncrementalArgs<'_>>,
        db: &Path,
    ) -> Self {
        match (mir_edges, incremental) {
            (Some(mir), Some(inc)) if mir.total > 0 => {
                Self::build_with_mir_incremental(chunks, mir, inc.changed_crates, db, inc.mir_edge_dir)
            }
            (Some(mir), _) if mir.total > 0 => Self::build_with_mir(chunks, mir),
            _ => Self::build(chunks),
        }
    }

    /// Encode and write graph to disk in a background thread.
    ///
    /// Both encoding and file I/O happen off the critical path.
    /// The graph is moved into the thread (no clone needed).
    /// If the process exits before the thread completes, the next run
    /// will detect a stale/missing graph.bin and rebuild.
    pub fn save_background(self, db: &Path) {
        let path = graph_cache_path(db);
        let _ = fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
        std::thread::spawn(move || {
            match bincode::encode_to_vec(&self, bincode::config::standard()) {
                Ok(bytes) => {
                    // Atomic write: write to temp file first, then rename.
                    // If the process exits mid-write, only the tmp file is
                    // left behind and graph.bin retains its previous version.
                    let tmp_path = path.with_extension("tmp.bin");
                    if let Err(e) = fs::write(&tmp_path, bytes) {
                        eprintln!("[graph] background save write failed: {e}");
                        return;
                    }
                    if let Err(e) = fs::rename(&tmp_path, &path) {
                        eprintln!("[graph] background save rename failed: {e}");
                        let _ = fs::remove_file(&tmp_path);
                    }
                }
                Err(e) => {
                    eprintln!("[graph] encode failed, skipping save: {e}");
                }
            }
        });
    }

    // ── Persistence ─────────────────────────────────────────────────

    pub fn save(&self, db: &Path) -> Result<()> {
        let path = graph_cache_path(db);
        let _ = fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
        let bytes = bincode::encode_to_vec(self, bincode::config::standard())
            .context("failed to encode call graph")?;
        fs::write(&path, bytes).with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn load(db: &Path) -> Option<Self> {
        let path = graph_cache_path(db);
        // No mtime comparison — save_background is async so graph.bin's
        // mtime may legitimately be older than payload.dat. Instead we
        // rely on version hash (GRAPH_SOURCE_HASH) for validity. If the
        // background save was interrupted, the tmp file won't have been
        // renamed, so graph.bin either holds a valid previous version or
        // doesn't exist → rebuild.
        let bytes = fs::read(&path).ok()?;
        let (graph, _): (Self, _) = bincode::decode_from_slice(&bytes, bincode::config::standard()).ok()?;
        if graph.version != GRAPH_SOURCE_HASH { return None; }
        Some(graph)
    }

    pub fn len(&self) -> usize { self.names.len() }
    pub fn is_empty(&self) -> bool { self.names.is_empty() }

    pub fn global_aliases(&self) -> (std::collections::BTreeMap<String, String>, Vec<(String, String)>) {
        let all: Vec<&str> = self.files.iter().map(|f| crate::helpers::relative_path(f)).collect();
        crate::helpers::build_path_aliases(&all)
    }
}

fn graph_cache_path(db: &Path) -> std::path::PathBuf {
    db.join("cache").join("graph.bin")
}
