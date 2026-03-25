
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rude_db::{PayloadValue, StorageEngine};

use crate::data::parse::{self, ParsedChunk};

const CHUNKS_CACHE_VERSION: u8 = 1;

pub fn load_chunks(path: &Path) -> Result<Vec<ParsedChunk>> {
    // Ensure project root is set for path normalization (idempotent).
    ensure_project_root(path);

    let cache = cache_path(path);
    // Use store.db mtime — the SQLite database file.
    let db_mtime = fs::metadata(path.join("store.db"))
        .and_then(|m| m.modified())
        .ok();

    // Try cache hit: version prefix byte + bincode payload.
    if let Some(db_t) = db_mtime
        && let Ok(cache_meta) = fs::metadata(&cache)
        && let Ok(cache_t) = cache_meta.modified()
        && cache_t >= db_t
        && let Ok(bytes) = fs::read(&cache)
        && bytes.first() == Some(&CHUNKS_CACHE_VERSION)
    {
        let config = bincode::config::standard();
        if let Ok((chunks, _)) =
            bincode::decode_from_slice::<Vec<ParsedChunk>, _>(&bytes[1..], config)
        {
            eprintln!("  chunks.bin cache hit: {} chunks from {:.1}MB", chunks.len(), bytes.len() as f64 / 1_048_576.0);
            return Ok(chunks);
        }
    }

    // Cache miss — try mir.db direct path first, then text-based fallback
    let chunks = load_chunks_from_mir_db(path)
        .ok()
        .filter(|c| !c.is_empty())
        .map_or_else(|| load_chunks_from_db(path), Ok)?;
    save_chunks_cache(&cache, &chunks);
    Ok(chunks)
}

pub fn load_chunks_from_mir_db(db_path: &Path) -> Result<Vec<ParsedChunk>> {
    let mir_db = crate::mir_edges::mir_db_path(db_path);
    if !mir_db.exists() {
        return load_chunks_from_db(db_path);
    }
    let mir_chunks = crate::mir_edges::MirEdgeMap::load_chunks_from_sqlite(&mir_db, None)?;
    Ok(crate::mir_edges::mir_chunks_to_parsed(&mir_chunks))
}

pub fn load_chunks_from_db(path: &Path) -> Result<Vec<ParsedChunk>> {
    let engine = StorageEngine::open(path)
        .with_context(|| format!("failed to open database at {}", path.display()))?;

    let rows = engine
        .iter_all()
        .with_context(|| format!("failed to read chunks from {}", path.display()))?;

    let mut chunks = Vec::with_capacity(rows.len());
    for (_, payload, text) in &rows {
        if let Some(mut chunk) = parse::parse_chunk(text) {
            if let Some(PayloadValue::StringList(imports)) = payload.custom.get("imports") {
                chunk.imports.clone_from(imports);
            }
            chunks.push(chunk);
        }
    }
    Ok(chunks)
}

pub fn save_chunks_cache(path: &Path, chunks: &[ParsedChunk]) {
    let config = bincode::config::standard();
    // Prepend version byte, then encode chunks.
    let mut bytes = vec![CHUNKS_CACHE_VERSION];
    if let Ok(chunk_bytes) = bincode::encode_to_vec(chunks, config) {
        bytes.extend_from_slice(&chunk_bytes);
        let _ = fs::write(path, bytes);
    }
}

pub fn load_chunks_from_cache(db: &Path) -> Option<Vec<ParsedChunk>> {
    let cache = cache_path(db);
    let bytes = fs::read(&cache).ok()?;
    if bytes.first() != Some(&CHUNKS_CACHE_VERSION) {
        return None;
    }
    let config = bincode::config::standard();
    let (chunks, _) = bincode::decode_from_slice::<Vec<ParsedChunk>, _>(&bytes[1..], config).ok()?;
    Some(chunks)
}

pub fn cache_path(db: &Path) -> PathBuf {
    db.join("cache").join("chunks.bin")
}

fn ensure_project_root(db: &Path) {
    use rude_db::DbConfig;
    if let Ok(config) = DbConfig::load(db) {
        if let Some(ref input_path) = config.input_path {
            parse::set_project_root(std::path::Path::new(input_path));
        }
    }
}
pub fn load_or_build_graph(db: &Path) -> Result<crate::graph::CallGraph> {
    let (g, _) = load_or_build_graph_with_chunks(db)?;
    Ok(g)
}

pub fn load_or_build_graph_with_chunks(
    db: &Path,
) -> Result<(crate::graph::CallGraph, Option<Vec<ParsedChunk>>)> {
    if let Some(g) = crate::graph::CallGraph::load(db) {
        return Ok((g, None));
    }

    let chunks = load_chunks(db)?;

    // Try MIR edges first (100% accurate), fall back to name-resolve.
    let mir_edges_dir = db.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."))
        .join("target")
        .join("mir-edges");
    let mir_edges = if mir_edges_dir.exists() {
        crate::mir_edges::MirEdgeMap::from_sqlite(&crate::mir_edges::mir_db_path(db.parent().unwrap_or(db)), None).ok()
    } else {
        None
    };

    if mir_edges.as_ref().map_or(false, |m| m.total > 0) {
        eprintln!("[graph] Rebuilding with MIR edges ({} total)...", mir_edges.as_ref().unwrap().total);
    } else {
        eprintln!("[graph] Building graph (name-resolve fallback)...");
    }
    let g = crate::graph::CallGraph::build_only(&chunks, mir_edges.as_ref(), None, db);

    let _ = g.save(db);
    Ok((g, Some(chunks)))
}
