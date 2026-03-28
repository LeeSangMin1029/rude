use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use rude_db::file_index;
use rude_db::DbConfig;
use rude_db::StorageEngine;
use rude_util::is_interrupted;

use crate::commands::add::{ingest_mir, write_chunks, CodeChunkEntry};
use super::mir::{run_mir_analysis, run_sub_workspaces, to_crate_filter};
use super::scan::{scan_files_fast, lang_summary, prebuild_caches, is_profiling, prof};

const TEXT_ONLY_DIM: usize = 1;
const TEXT_ONLY_MODEL: &str = "text-only";

pub fn run(input_path: PathBuf, exclude: &[String]) -> Result<()> {
    let db_path = crate::db().to_path_buf();
    use rude_util::get_file_mtime;

    rude_intel::parse::set_project_root(&input_path);

    println!("Indexing code: {}", input_path.display());
    println!("Database:      {}", db_path.display());

    let all_files = prof!("scan_files", scan_files_fast(&input_path, exclude));
    if !is_profiling() { tracing::debug!("scan: {} files", all_files.len()); }
    if all_files.is_empty() {
        anyhow::bail!("No supported code files found in {}", input_path.display());
    }

    let current_sources: std::collections::HashSet<String> =
        all_files.iter().map(|f| rude_util::normalize_source(f)).collect();

    let file_idx_engine = if db_path.join("store.db").exists() {
        Some(StorageEngine::open(&db_path)
            .with_context(|| "failed to open store.db for file_index")?)
    } else {
        None
    };
    let mut file_idx = match &file_idx_engine {
        Some(e) => file_index::load_file_index(e)?,
        None => file_index::FileIndex::new(),
    };
    drop(file_idx_engine);
    let code_files: Vec<_> = all_files.iter().filter(|f| {
        let source = rude_util::normalize_source(f);
        match file_idx.get_file(&source) {
            Some(entry) => {
                if get_file_mtime(*f).is_none_or(|m| m == entry.mtime) { return false; }
                entry.content_hash.is_none_or(|prev| {
                    rude_util::content_hash(f).is_ok_and(|cur| cur != prev)
                })
            }
            None => true,
        }
    }).collect();
    let source_cache: HashMap<std::path::PathBuf, String> = code_files.iter()
        .map(|f| ((*f).clone(), rude_util::normalize_source(f))).collect();

    let missing_crates = detect_missing_from_cache(&input_path);
    if code_files.is_empty() && missing_crates.is_empty() {
        println!("No files changed. Nothing to update.");
        return Ok(());
    }

    println!("Files: {} ({})", code_files.len(), lang_summary(&code_files));

    let engine = if db_path.exists() {
        StorageEngine::open_exclusive(&db_path)
            .with_context(|| format!("Failed to open database at {}", db_path.display()))?
    } else {
        println!("New database: {} (dim={TEXT_ONLY_DIM})", db_path.display());
        for rel in &["target/mir-edges", "target/mir-check/debug/.fingerprint"] {
            let p = input_path.join(rel);
            if p.exists() { let _ = std::fs::remove_dir_all(&p); }
        }
        let engine = StorageEngine::open_exclusive(&db_path)
            .with_context(|| format!("Failed to create database at {}", db_path.display()))?;
        DbConfig { code: true, embedded: false, embed_model: Some(TEXT_ONLY_MODEL.to_owned()), ..DbConfig::default() }
            .save(&engine)?;
        engine
    };

    if let Ok(mut config) = DbConfig::load(&engine) {
        config.code = true;
        config.embedded = false;
        config.input_path = Some(rude_util::safe_canonicalize(&input_path).to_string_lossy().into_owned());
        let _ = config.save(&engine);
    }

    let t0 = std::time::Instant::now();
    let mut entries: Vec<CodeChunkEntry> = Vec::new();
    let mut file_metadata_map: HashMap<String, (u64, u64, Vec<u64>)> = HashMap::new();

    let mir_out_dir = input_path.join("target").join("mir-edges");
    std::fs::create_dir_all(&mir_out_dir).ok();
    let mir_db = rude_intel::mir_edges::mir_db_path(&input_path);

    let incremental_crates = prof!("mir_analysis", run_mir_analysis(&input_path, &mir_db, &code_files, &missing_crates)?);
    prof!("sub_workspaces", run_sub_workspaces(&input_path, &mir_db, &code_files).ok());

    let mir_chunks = prof!("load_sqlite", rude_intel::mir_edges::MirEdgeMap::load_chunks_from_sqlite(
        &mir_db, to_crate_filter(&incremental_crates).as_deref(),
    ).context("failed to load MIR chunks")?);

    prof!("ingest_mir", ingest_mir(&mir_chunks, &db_path, &mut entries, &mut file_metadata_map, None)?);
    tracing::debug!("chunk: {:.1}s ({} chunks)", t0.elapsed().as_secs_f64(), entries.len());

    println!("Symbols: {} (functions, structs, enums, ...)", entries.len());
    let inserted = prof!("write_chunks", write_chunks(&entries, &file_metadata_map, &mut file_idx, true)?);
    if !is_profiling() { println!("\nInserted {inserted} chunks in 0.00s"); }

    for f in &code_files {
        let source = source_cache.get(*f).cloned().unwrap_or_default();
        if !file_metadata_map.contains_key(&source) {
            if let Some(mtime) = get_file_mtime(*f) {
                let size = file_index::get_file_size(*f).unwrap_or(0);
                let existing_chunk_ids = file_idx
                    .get_file(&source)
                    .map(|e| e.chunk_ids.clone())
                    .unwrap_or_default();
                let hash = rude_util::content_hash(f).unwrap_or(0);
                file_idx.update_file(source, mtime, size, existing_chunk_ids, Some(hash));
            }
        }
    }
    file_index::save_file_index(&engine, &file_idx)?;

    let deleted: Vec<String> = file_idx.files.keys()
        .filter(|p| !current_sources.contains(p.as_str()))
        .cloned()
        .collect();
    if !deleted.is_empty() {
        for path in &deleted { file_idx.files.remove(path); }
        file_index::save_file_index(&engine, &file_idx)?;
        eprintln!("Removed {} deleted file(s) from index", deleted.len());
    }

    if is_interrupted() {
        println!("\nOperation interrupted. Partial data may have been inserted.");
        return Ok(());
    }

    if inserted == 0 && deleted.is_empty() {
        println!("No changes. Database is up to date.");
    } else {
        println!("\nDone! Code DB ready: {}", db_path.display());
        tracing::debug!("Use: rude context/blast/symbols/dupes {}", db_path.display());
        prof!("checkpoint", engine.checkpoint().ok());
        drop(engine);
        prebuild_caches(&entries, &incremental_crates);
    }

    Ok(())
}

fn detect_missing_from_cache(input_path: &std::path::Path) -> Vec<String> {
    let cached_crates: std::collections::HashSet<String> = rude_intel::loader::cached_crate_names()
        .into_iter().collect();
    let mir_crates: std::collections::HashSet<String> = rude_intel::mir_edges::detect_missing_edge_crates(input_path)
        .into_iter().collect();
    // also check crates in mir.db but not in chunks cache
    let all_mir = rude_intel::mir_edges::mir_crate_names(input_path);
    all_mir.into_iter()
        .filter(|c| !cached_crates.contains(c))
        .chain(mir_crates)
        .collect::<std::collections::HashSet<_>>()
        .into_iter().collect()
}
