
use std::path::Path;

use anyhow::Result;

use crate::data::parse::{self, ParsedChunk};

const CHUNKS_CACHE_VERSION: u8 = 1;

pub fn load_chunks() -> Result<Vec<ParsedChunk>> {
    let path = crate::db();
    if let Ok(engine) = rude_db::StorageEngine::open(path) {
        ensure_project_root_with_engine(&engine);
        if let Some(chunks) = load_chunks_from_cache_with_engine(&engine) {
            eprintln!("  chunks cache hit: {} chunks", chunks.len());
            return Ok(chunks);
        }
    } else {
        ensure_project_root(path);
    }
    let chunks = load_chunks_from_mir_db(path)?;
    save_chunks_cache(&chunks);
    Ok(chunks)
}

pub fn load_chunks_from_mir_db(db_path: &Path) -> Result<Vec<ParsedChunk>> {
    let mir_db = crate::mir_edges::mir_db_path(db_path.parent().unwrap_or(db_path));
    if !mir_db.exists() {
        anyhow::bail!("mir.db not found at {}", mir_db.display());
    }
    let mir_chunks = crate::mir_edges::MirEdgeMap::load_chunks_from_sqlite(&mir_db, None)?;
    let parsed = mir_chunks.iter().map(|mc| mc.to_parsed()).collect();
    Ok(parsed)
}

pub fn save_chunks_cache(chunks: &[ParsedChunk]) {
    let Ok(engine) = rude_db::StorageEngine::open(crate::db()) else { return };
    save_chunks_cache_with_engine(&engine, chunks);
}

pub fn save_chunks_cache_with_engine(engine: &rude_db::StorageEngine, chunks: &[ParsedChunk]) {
    let config = bincode::config::standard();
    let mut bytes = vec![CHUNKS_CACHE_VERSION];
    if let Ok(cb) = bincode::encode_to_vec(chunks, config) {
        bytes.extend_from_slice(&cb);
        let _ = engine.set_cache("chunks", &bytes);
    }
}

#[tracing::instrument(skip_all)]
pub fn save_chunks_cache_for(chunks: &[ParsedChunk], changed_crates: Option<&[&str]>) {
    let db = crate::db();
    let Ok(engine) = rude_db::StorageEngine::open(db) else { return };
    let config = bincode::config::standard();
    let mut by_crate: std::collections::HashMap<&str, Vec<ParsedChunk>> = std::collections::HashMap::new();
    for c in chunks {
        let key = if c.crate_name.is_empty() { "(root)" } else { &c.crate_name };
        by_crate.entry(key).or_default().push(c.clone());
    }
    let crate_names_snapshot: Vec<String> = by_crate.keys().map(|s| s.to_string()).collect();
    for (name, mut crate_chunks) in by_crate {
        if let Some(changed) = changed_crates {
            if !changed.iter().any(|c| c.replace('-', "_") == name.replace('-', "_")) { continue; }
        }
        let key = format!("chunks:{name}");
        if changed_crates.is_some() {
            if let Ok(Some(existing_bytes)) = engine.get_cache(&key) {
                if existing_bytes.first() == Some(&CHUNKS_CACHE_VERSION) {
                    if let Ok((existing, _)) = bincode::decode_from_slice::<Vec<ParsedChunk>, _>(&existing_bytes[1..], config) {
                        let new_names: std::collections::HashSet<(&str, &str)> =
                            crate_chunks.iter().map(|c| (c.file.as_str(), c.name.as_str())).collect();
                        let retained: Vec<ParsedChunk> = existing.into_iter()
                            .filter(|c| !new_names.contains(&(c.file.as_str(), c.name.as_str())))
                            .collect();
                        if !retained.is_empty() {
                            crate_chunks.extend(retained);
                            crate_chunks.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.lines.cmp(&b.lines)));
                        }
                    }
                }
            }
        }
        let mut bytes = vec![CHUNKS_CACHE_VERSION];
        if let Ok(cb) = bincode::encode_to_vec(&crate_chunks, config) {
            bytes.extend_from_slice(&cb);
            let _ = engine.set_cache(&key, &bytes);
        }
    }
    let mut all_names: std::collections::HashSet<String> = crate_names_snapshot.into_iter().collect();
    if changed_crates.is_some() {
        if let Ok(Some(idx_bytes)) = engine.get_cache("chunks:_index") {
            if let Ok((existing, _)) = bincode::decode_from_slice::<Vec<String>, _>(&idx_bytes, config) {
                all_names.extend(existing);
            }
        }
    }
    let names_vec: Vec<&str> = all_names.iter().map(|s| s.as_str()).collect();
    if let Ok(b) = bincode::encode_to_vec(&names_vec, config) {
        let _ = engine.set_cache("chunks:_index", &b);
    }
}

pub fn load_chunks_from_cache() -> Option<Vec<ParsedChunk>> {
    let engine = rude_db::StorageEngine::open(crate::db()).ok()?;
    load_chunks_from_cache_with_engine(&engine)
}

pub fn load_chunks_from_cache_with_engine(engine: &rude_db::StorageEngine) -> Option<Vec<ParsedChunk>> {
    let config = bincode::config::standard();
    if let Ok(Some(idx_bytes)) = engine.get_cache("chunks:_index") {
        if let Ok((crate_names, _)) = bincode::decode_from_slice::<Vec<String>, _>(&idx_bytes, config) {
            let mut all = Vec::new();
            for name in &crate_names {
                if let Ok(Some(bytes)) = engine.get_cache(&format!("chunks:{name}")) {
                    if bytes.first() == Some(&CHUNKS_CACHE_VERSION) {
                        if let Ok((chunks, _)) = bincode::decode_from_slice::<Vec<ParsedChunk>, _>(&bytes[1..], config) {
                            all.extend(chunks);
                        }
                    }
                }
            }
            if !all.is_empty() {
                all.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.lines.cmp(&b.lines)));
                return Some(all);
            }
        }
    }
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
pub fn load_or_build_graph() -> Result<crate::graph::CallGraph> {
    let db = crate::db();
    if let Some(g) = crate::graph::CallGraph::load() {
        return Ok(g);
    }
    let chunks = load_chunks()?;
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
    let g = crate::graph::CallGraph::build_only(chunks, mir_edges.as_ref(), None);
    let _ = g.save();
    Ok(g)
}

pub fn cached_crate_names() -> Vec<String> {
    let db = crate::db();
    let Ok(engine) = rude_db::StorageEngine::open(db) else { return Vec::new() };
    let config = bincode::config::standard();
    engine.get_cache("chunks:_index").ok().flatten()
        .and_then(|b| bincode::decode_from_slice::<Vec<String>, _>(&b, config).ok())
        .map(|(v, _)| v)
        .unwrap_or_default()
}

#[cfg(test)]
pub fn save_chunks_cache_at(db: &Path, chunks: &[ParsedChunk]) {
    let Ok(engine) = rude_db::StorageEngine::open(db) else { return };
    let config = bincode::config::standard();
    let mut bytes = vec![CHUNKS_CACHE_VERSION];
    if let Ok(cb) = bincode::encode_to_vec(chunks, config) {
        bytes.extend_from_slice(&cb);
        let _ = engine.set_cache("chunks", &bytes);
    }
}
