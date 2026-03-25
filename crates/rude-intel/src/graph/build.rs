use std::path::Path;

use anyhow::{Context, Result};

use crate::graph::edge_resolve::{self, ChunkIndex, ResolvedEdges};
use crate::graph::index_tables;
use crate::mir_edges::MirEdgeMap;
use crate::data::parse::ParsedChunk;

const GRAPH_SOURCE_HASH: &str = env!("GRAPH_SOURCE_HASH");

pub use crate::graph::index_tables::{is_test_path, is_test_chunk};

pub struct IncrementalArgs<'a> {
    pub changed_crates: &'a [String],
    pub mir_edge_dir: &'a Path,
}

#[derive(bincode::Encode, bincode::Decode)]
pub struct Edges {
    version: String,
    pub name_index: Vec<(String, u32)>,
    pub callees: Vec<Vec<u32>>,
    pub callers: Vec<Vec<u32>>,
    pub is_test: Vec<bool>,
    pub trait_impls: Vec<Vec<u32>>,
    pub impl_of_trait: Vec<Option<u32>>,
    pub fn_trait_impl: Vec<Option<u32>>,
    pub call_sites: Vec<Vec<(u32, u32)>>,
    pub field_access_index: Vec<(String, Vec<u32>)>,
}

pub struct CallGraph {
    pub chunks: Vec<ParsedChunk>,
    pub edges: Edges,
}

impl std::ops::Deref for CallGraph {
    type Target = Edges;
    fn deref(&self) -> &Edges { &self.edges }
}

impl CallGraph {

    fn assemble(chunks: Vec<ParsedChunk>, index: &ChunkIndex, adj: ResolvedEdges, ) -> Self {
        let t = std::time::Instant::now();
        let owner_field_types = index_tables::collect_owner_field_types(&chunks);

        let len = chunks.len();
        let mut is_test = Vec::with_capacity(len);
        let mut name_index = Vec::with_capacity(len);

        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        let kinds: Vec<&str> = chunks.iter().map(|c| c.kind.as_str()).collect();

        for (i, c) in chunks.iter().enumerate() {
            name_index.push((c.name.to_lowercase(), i as u32));
            is_test.push(is_test_chunk(c));
        }
        name_index.sort_by(|a, b| a.0.cmp(&b.0));

        let (trait_impls, field_access_index) = rayon::join(
            || index_tables::build_trait_impls(&names, &kinds, &index.exact, &index.short),
            || index_tables::build_field_access_index(&chunks, &owner_field_types),
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
            chunks,
            edges: Edges {
                version: GRAPH_SOURCE_HASH.to_owned(),
                name_index, callees: adj.callees, callers: adj.callers,
                is_test, trait_impls, impl_of_trait, fn_trait_impl,
                call_sites: adj.call_sites, field_access_index,
            },
        }
    }

    /// Name-based graph construction used only in tests (no MIR available).
    pub fn build(chunks: &[ParsedChunk]) -> Self {
        let index = ChunkIndex::build(chunks);
        let adj = edge_resolve::resolve_by_name_test(chunks, &index);
        Self::assemble(chunks.to_vec(), &index, adj)
    }

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

    pub fn rebuild(
        db: &Path,
        chunks: Vec<ParsedChunk>,
        mir_edges: Option<&MirEdgeMap>,
        incremental: Option<IncrementalArgs<'_>>,
    ) -> Result<Self> {
        let graph = Self::build_only(chunks, mir_edges, incremental, db);
        graph.save(db)?;
        Ok(graph)
    }

    pub fn build_only(
        chunks: Vec<ParsedChunk>,
        mir_edges: Option<&MirEdgeMap>,
        incremental: Option<IncrementalArgs<'_>>,
        db: &Path,
    ) -> Self {
        let t0 = std::time::Instant::now();
        let index = ChunkIndex::build(&chunks);
        let (adj, label) = match (mir_edges, incremental) {
            (Some(mir), Some(inc)) if mir.total > 0 => {
                let adj = edge_resolve::resolve_incremental(
                    &chunks, &index, mir, inc.changed_crates, db, inc.mir_edge_dir,
                );
                (adj, "mir-incremental")
            }
            (Some(mir), _) if mir.total > 0 => {
                (edge_resolve::resolve_with_mir(&chunks, &index, mir), "mir-resolve")
            }
            _ => {
                (edge_resolve::ResolvedEdges::empty(chunks.len()), "no-mir")
            }
        };
        eprintln!("      [graph] {label}: {:.1}ms ({} chunks)", t0.elapsed().as_secs_f64() * 1000.0, chunks.len());
        Self::assemble(chunks, &index, adj)
    }


    pub fn save_background(self, db: &Path) {
        let db = db.to_path_buf();
        std::thread::spawn(move || {
            if let Ok(bytes) = bincode::encode_to_vec(&self.edges, bincode::config::standard()) {
                if let Ok(engine) = rude_db::StorageEngine::open(&db) {
                    let _ = engine.set_cache("graph", &bytes);
                }
            }
        });
    }

    pub fn save(&self, db: &Path) -> Result<()> {
        let engine = rude_db::StorageEngine::open(db)?;
        self.save_with_engine(&engine)
    }

    pub fn save_with_engine(&self, engine: &rude_db::StorageEngine) -> Result<()> {
        let bytes = bincode::encode_to_vec(&self.edges, bincode::config::standard())
            .context("failed to encode edges")?;
        engine.set_cache("graph", &bytes)
    }

    pub fn load(db: &Path) -> Option<Self> {
        let engine = rude_db::StorageEngine::open(db).ok()?;
        Self::load_with_engine(&engine)
    }

    pub fn load_with_engine(engine: &rude_db::StorageEngine) -> Option<Self> {
        let bytes = engine.get_cache("graph").ok()??;
        let (edges, _): (Edges, _) = bincode::decode_from_slice(&bytes, bincode::config::standard()).ok()?;
        if edges.version != GRAPH_SOURCE_HASH { return None; }
        let chunks = crate::analysis::loader::load_chunks_from_cache_with_engine(engine)?;
        if chunks.len() != edges.callees.len() { return None; }
        Some(Self { chunks, edges })
    }

    pub fn len(&self) -> usize { self.chunks.len() }
    pub fn is_empty(&self) -> bool { self.chunks.is_empty() }

    pub fn global_aliases(&self) -> (std::collections::BTreeMap<String, String>, Vec<(String, String)>) {
        let all: Vec<&str> = self.chunks.iter().map(|c| crate::helpers::relative_path(&c.file)).collect();
        crate::helpers::build_path_aliases(&all)
    }
}
