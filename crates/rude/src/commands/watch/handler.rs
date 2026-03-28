use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use rude_db::file_index;
use rude_util::normalize_source;

pub(super) fn process_changes(changed: &[PathBuf], db_path: &Path, input_path: &Path) {
    let t = std::time::Instant::now();
    let file_names: Vec<String> = changed
        .iter()
        .filter_map(|p| p.file_name())
        .filter_map(|n| n.to_str())
        .map(|s| s.to_owned())
        .collect();
    eprintln!(
        "[watch] {} file(s) changed: {}",
        changed.len(),
        file_names.join(", ")
    );

    let crates = rude_intel::mir_edges::detect_changed_crates(input_path, changed);
    if crates.is_empty() {
        eprintln!("[watch] no crate detected for changed files");
        return;
    }
    eprintln!("[watch] crate(s): {}", crates.join(", "));

    let crate_refs: Vec<&str> = crates.iter().map(|s| s.as_str()).collect();
    if let Err(e) = rude_intel::mir_edges::run_mir_direct(input_path, None, &crate_refs, true) {
        eprintln!("[watch] mir-callgraph failed: {e}");
        return;
    }

    let mir_db = rude_intel::mir_edges::mir_db_path(input_path);
    let mir_chunks = match rude_intel::mir_edges::MirEdgeMap::load_chunks_from_sqlite(&mir_db, None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[watch] failed to load MIR chunks from sqlite: {e}");
            return;
        }
    };

    let changed_sources: HashSet<String> = changed.iter().map(|f| normalize_source(f)).collect();

    let mut entries = Vec::new();
    let mut file_metadata_map = HashMap::new();
    if let Err(e) = crate::commands::add::ingest_mir(
        &mir_chunks,
        db_path,
        &mut entries,
        &mut file_metadata_map,
        Some(&changed_sources),
    ) {
        eprintln!("[watch] chunk_from_mir failed: {e}");
        return;
    }

    if entries.is_empty() {
        eprintln!("[watch] no chunks to update");
        return;
    }

    if let Err(e) = update_db(db_path, &entries, &file_metadata_map) {
        eprintln!("[watch] DB update failed: {e}");
        return;
    }

    let mir_out_dir = input_path.join("target").join("mir-edges");
    let mir_edge_map = rude_intel::mir_edges::MirEdgeMap::from_sqlite(&mir_db, None).unwrap_or_default();
    if let Err(e) = rebuild_graph_cache(db_path, &mir_out_dir, &mir_edge_map, &crates) {
        eprintln!("[watch] graph rebuild failed: {e}");
        return;
    }

    eprintln!(
        "[watch] updated: {} chunks ({:.1}s)\n",
        entries.len(),
        t.elapsed().as_secs_f64()
    );
}

fn update_db(
    db_path: &Path,
    entries: &[crate::commands::add::CodeChunkEntry],
    file_metadata_map: &HashMap<String, (u64, u64, Vec<u64>)>,
) -> Result<()> {
    let engine = rude_db::StorageEngine::open_exclusive(db_path)
        .context("failed to open DB for writing")?;

    let mut file_idx = file_index::load_file_index(&engine)?;
    crate::commands::add::write_chunks(entries, file_metadata_map, &mut file_idx, false)?;
    engine.checkpoint()?;
    file_index::save_file_index(&engine, &file_idx)?;

    Ok(())
}

fn rebuild_graph_cache(
    db_path: &Path,
    mir_out_dir: &Path,
    mir_edge_map: &rude_intel::mir_edges::MirEdgeMap,
    changed_crates: &[String],
) -> Result<()> {
    let chunks = rude_intel::loader::load_chunks(db_path)?;

    rude_intel::loader::save_chunks_cache(db_path, &chunks);

    let incremental = rude_intel::graph::IncrementalArgs {
        changed_crates,
        mir_edge_dir: mir_out_dir,
    };
    let graph = rude_intel::graph::CallGraph::rebuild(
        db_path, chunks, Some(mir_edge_map), Some(incremental),
    )?;
    eprintln!(
        "[watch] graph: {} nodes, {} edges",
        graph.len(),
        graph.callees.iter().map(Vec::len).sum::<usize>()
    );

    Ok(())
}
