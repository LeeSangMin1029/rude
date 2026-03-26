
use std::path::Path;

use anyhow::Result;

use crate::data::parse::{self, ParsedChunk};

const CHUNKS_CACHE_VERSION: u8 = 1;

pub fn load_chunks(path: &Path) -> Result<Vec<ParsedChunk>> {
    if let Ok(engine) = rude_db::StorageEngine::open(path) {
        // Ensure project root is set for path normalization (idempotent).
        ensure_project_root_with_engine(&engine);

        // Try cache hit from sqlite kv_cache.
        if let Some(chunks) = load_chunks_from_cache_with_engine(&engine) {
            eprintln!("  chunks cache hit: {} chunks", chunks.len());
            return Ok(chunks);
        }
    } else {
        ensure_project_root(path);
    }

    // Cache miss — load from mir.db
    let chunks = load_chunks_from_mir_db(path)?;
    save_chunks_cache(path, &chunks);
    Ok(chunks)
}

pub fn load_chunks_from_mir_db(db_path: &Path) -> Result<Vec<ParsedChunk>> {
    let mir_db = crate::mir_edges::mir_db_path(db_path);
    if !mir_db.exists() {
        anyhow::bail!("mir.db not found at {}", mir_db.display());
    }
    let mir_chunks = crate::mir_edges::MirEdgeMap::load_chunks_from_sqlite(&mir_db, None)?;
    let parsed = mir_chunks.iter().map(|mc| mc.to_parsed()).collect();
    Ok(parsed)
}

#[tracing::instrument(skip_all)]
pub fn save_chunks_cache(db: &Path, chunks: &[ParsedChunk]) {
    let Ok(engine) = rude_db::StorageEngine::open(db) else { return };
    let config = bincode::config::standard();
    let mut bytes = vec![CHUNKS_CACHE_VERSION];
    if let Ok(cb) = bincode::encode_to_vec(chunks, config) {
        bytes.extend_from_slice(&cb);
        let _ = engine.set_cache("chunks", &bytes);
    }
}

#[tracing::instrument(skip_all)]
pub fn save_chunks_cache_for(db: &Path, chunks: &[ParsedChunk], changed_crates: Option<&[&str]>) {
    let Ok(engine) = rude_db::StorageEngine::open(db) else { return };
    let config = bincode::config::standard();

    // Group by crate
    let mut by_crate: std::collections::HashMap<String, Vec<&ParsedChunk>> = std::collections::HashMap::new();
    for c in chunks {
        let crate_name = crate::helpers::extract_crate_name(&c.file);
        by_crate.entry(crate_name).or_default().push(c);
    }

    // Save only changed crates (or all if changed_crates is None)
    for (name, crate_chunks) in &by_crate {
        if let Some(changed) = changed_crates {
            if !changed.iter().any(|c| c.replace('-', "_") == name.replace('-', "_")) { continue; }
        }
        let key = format!("chunks:{name}");
        let mut bytes = vec![CHUNKS_CACHE_VERSION];
        if let Ok(cb) = bincode::encode_to_vec(crate_chunks, config) {
            bytes.extend_from_slice(&cb);
            let _ = engine.set_cache(&key, &bytes);
        }
    }
    // Save crate list
    let crate_names: Vec<&str> = by_crate.keys().map(|s| s.as_str()).collect();
    if let Ok(b) = bincode::encode_to_vec(&crate_names, config) {
        let _ = engine.set_cache("chunks:_index", &b);
    }
}

pub fn load_chunks_from_cache(db: &Path) -> Option<Vec<ParsedChunk>> {
    let engine = rude_db::StorageEngine::open(db).ok()?;
    load_chunks_from_cache_with_engine(&engine)
}

pub fn load_chunks_from_cache_with_engine(engine: &rude_db::StorageEngine) -> Option<Vec<ParsedChunk>> {
    let config = bincode::config::standard();
    let bytes = engine.get_cache("chunks").ok()??;
    if bytes.first() != Some(&CHUNKS_CACHE_VERSION) { return None; }
    let (chunks, _) = bincode::decode_from_slice::<Vec<ParsedChunk>, _>(&bytes[1..], config).ok()?;
    Some(chunks)
}

fn ensure_project_root(db: &Path) {
    if let Ok(engine) = rude_db::StorageEngine::open(db) {
        ensure_project_root_with_engine(&engine);
    }
}

fn ensure_project_root_with_engine(engine: &rude_db::StorageEngine) {
    use rude_db::DbConfig;
    if let Ok(config) = DbConfig::load(engine) {
        if let Some(ref input_path) = config.input_path {
            parse::set_project_root(std::path::Path::new(input_path));
        }
    }
}
pub fn load_or_build_graph(db: &Path) -> Result<crate::graph::CallGraph> {
    if let Some(g) = crate::graph::CallGraph::load(db) {
        return Ok(g);
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
    let g = crate::graph::CallGraph::build_only(chunks, mir_edges.as_ref(), None, db);

    let _ = g.save(db);
    Ok(g)
}
