use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use rude_db::file_index;
use rude_db::file_utils::scan_files;
use rude_db::DbConfig;
use rude_db::is_interrupted;
use rude_db::StorageEngine;

use super::{ingest_mir, write_chunks, CodeChunkEntry};

const TEXT_ONLY_DIM: usize = 1;
const TEXT_ONLY_MODEL: &str = "text-only";

#[tracing::instrument(skip_all)]
pub fn run(input_path: PathBuf, exclude: &[String]) -> Result<()> {
    let db_path = crate::db().to_path_buf();
    use rude_db::file_utils::get_file_mtime;

    rude_intel::parse::set_project_root(&input_path);

    println!("Indexing code: {}", input_path.display());
    println!("Database:      {}", db_path.display());

    let t_scan = std::time::Instant::now();
    let all_files = scan_files_fast(&input_path, exclude);
    eprintln!("  scan: {:.1}ms ({} files)", t_scan.elapsed().as_secs_f64() * 1000.0, all_files.len());
    if all_files.is_empty() {
        anyhow::bail!("No supported code files found in {}", input_path.display());
    }

    let current_sources: std::collections::HashSet<String> =
        all_files.iter().map(|f| rude_db::file_utils::normalize_source(f)).collect();

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
        let source = rude_db::file_utils::normalize_source(f);
        match file_idx.get_file(&source) {
            Some(entry) => {
                if get_file_mtime(*f).is_none_or(|m| m == entry.mtime) { return false; }
                entry.content_hash.is_none_or(|prev| {
                    rude_db::file_utils::content_hash(f).is_ok_and(|cur| cur != prev)
                })
            }
            None => true,
        }
    }).collect();
    let source_cache: HashMap<std::path::PathBuf, String> = code_files.iter()
        .map(|f| ((*f).clone(), rude_db::file_utils::normalize_source(f))).collect();

    if code_files.is_empty() {
        println!("No files changed. Nothing to update.");
        return Ok(());
    }

    println!("Files: {} ({})", code_files.len(), lang_summary(&code_files));

    let mut engine = if db_path.exists() {
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
        if let Ok(canonical) = input_path.canonicalize() {
            let path_str = canonical.to_string_lossy();
            config.input_path = Some(rude_db::strip_unc_prefix(&path_str).to_owned());
        }
        let _ = config.save(&engine);
    }

    let t0 = std::time::Instant::now();
    let mut entries: Vec<CodeChunkEntry> = Vec::new();
    let mut file_metadata_map: HashMap<String, (u64, u64, Vec<u64>)> = HashMap::new();

    let mir_out_dir = input_path.join("target").join("mir-edges");
    std::fs::create_dir_all(&mir_out_dir).ok();
    let mir_db = rude_intel::mir_edges::mir_db_path(&input_path);

    let incremental_crates = run_mir_analysis(&input_path, &mir_db, &code_files)?;

    let mir_chunks = rude_intel::mir_edges::MirEdgeMap::load_chunks_from_sqlite(
        &mir_db, to_crate_filter(&incremental_crates).as_deref(),
    ).context("failed to load MIR chunks")?;

    let changed_sources: std::collections::HashSet<String> =
        code_files.iter().filter_map(|f| source_cache.get(*f).cloned()).collect();
    ingest_mir(&mir_chunks, &db_path, &mut entries, &mut file_metadata_map, Some(&changed_sources))?;
    eprintln!("  chunk: {:.1}s ({} chunks)", t0.elapsed().as_secs_f64(), entries.len());

    println!("Symbols: {} (functions, structs, enums, ...)", entries.len());
    let t_write = std::time::Instant::now();
    let inserted = write_chunks(&entries, &mut engine, &file_metadata_map, &mut file_idx, true)?;
    println!("\nInserted {inserted} chunks in {:.2}s", t_write.elapsed().as_secs_f64());

    // Record files that had no MIR chunks (0-chunk files) in the index so
    // their mtime/hash are tracked and they are not re-parsed next run.
    for f in &code_files {
        let source = source_cache.get(*f).cloned().unwrap_or_default();
        if !file_metadata_map.contains_key(&source) {
            if let Some(mtime) = get_file_mtime(*f) {
                let size = file_index::get_file_size(*f).unwrap_or(0);
                let existing_chunk_ids = file_idx
                    .get_file(&source)
                    .map(|e| e.chunk_ids.clone())
                    .unwrap_or_default();
                let hash = rude_db::file_utils::content_hash(f).unwrap_or(0);
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
        let mut del_count = 0usize;
        for path in &deleted {
            if let Some(entry) = file_idx.files.remove(path) {
                for id in entry.chunk_ids { let _ = engine.remove(id); del_count += 1; }
            }
        }
        if del_count > 0 {
            engine.checkpoint().ok();
            file_index::save_file_index(&engine, &file_idx)?;
            eprintln!("Removed {del_count} chunks from {} deleted file(s)", deleted.len());
        }
    }

    if is_interrupted() {
        println!("\nOperation interrupted. Partial data may have been inserted.");
        return Ok(());
    }

    if inserted == 0 && deleted.is_empty() {
        println!("No changes. Database is up to date.");
    } else {
        println!("\nDone! Code DB ready: {}", db_path.display());
        println!("Use: rude context/blast/symbols/dupes {}", db_path.display());
        engine.checkpoint().ok();
        drop(engine);
        let mir_edges = load_mir_edges(&mir_db, &mir_out_dir, &incremental_crates);
        prebuild_caches(&db_path, &entries, mir_edges.as_ref(), &mir_out_dir);
    }

    Ok(())
}

fn to_crate_filter(crates: &[String]) -> Option<Vec<&str>> {
    if crates.is_empty() { None } else { Some(crates.iter().map(String::as_str).collect()) }
}

fn lang_summary(files: &[&PathBuf]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for f in files {
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
        *counts.entry(rude_db::lang_for_ext(ext)).or_default() += 1;
    }
    let mut pairs: Vec<_> = counts.iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(a.1));
    pairs.iter().map(|(l, n)| format!("{l}:{n}")).collect::<Vec<_>>().join(", ")
}

fn run_mir_analysis(
    input_path: &std::path::Path,
    mir_db: &std::path::Path,
    code_files: &[&PathBuf],
) -> Result<Vec<String>> {
    let has_cached_edges = mir_db.exists();

    if !has_cached_edges {
        rude_intel::mir_edges::clear_mir_db(input_path, &[]).ok();
        rude_intel::mir_edges::run_mir_callgraph(input_path, None)
            .context("mir-callgraph failed — ensure nightly rustc and mir-callgraph are installed")?;
        return Ok(Vec::new());
    }

    let rust_changed: Vec<_> = code_files.iter()
        .filter(|f| f.extension().and_then(|e| e.to_str()) == Some("rs"))
        .collect();
    let mut changed_crates = rude_intel::mir_edges::detect_changed_crates(input_path, &rust_changed);

    let missing = rude_intel::mir_edges::detect_missing_edge_crates(input_path);
    if !missing.is_empty() {
        eprintln!("  [mir] missing edge files for: {}", missing.join(", "));
        for m in missing { if !changed_crates.contains(&m) { changed_crates.push(m); } }
    }

    if changed_crates.is_empty() { return Ok(Vec::new()); }

    let crate_refs: Vec<&str> = changed_crates.iter().map(|s| s.as_str()).collect();
    eprintln!("  [mir] incremental: {} crate(s) — {}", crate_refs.len(), crate_refs.join(", "));
    rude_intel::mir_edges::clear_mir_db(input_path, &crate_refs).ok();
    let rust_only = code_files.iter().all(|f| f.extension().and_then(|e| e.to_str()) == Some("rs"));
    rude_intel::mir_edges::run_mir_direct(input_path, None, &crate_refs, rust_only)
        .context("mir-callgraph incremental failed")?;
    Ok(changed_crates)
}

fn load_mir_edges(
    mir_db: &std::path::Path,
    mir_out_dir: &std::path::Path,
    incremental_crates: &[String],
) -> Option<rude_intel::mir_edges::MirEdgeMap> {
    if mir_db.exists() || mir_out_dir.exists() {
        rude_intel::mir_edges::MirEdgeMap::from_sqlite(mir_db, to_crate_filter(incremental_crates).as_deref()).ok()
    } else {
        None
    }
}

fn merge_chunks_cache(
    db_path: &std::path::Path,
    new_entries: &[CodeChunkEntry],
) -> Vec<rude_intel::parse::ParsedChunk> {
    let new_chunks: Vec<rude_intel::parse::ParsedChunk> = new_entries.iter()
        .map(|e| e.chunk.clone())
        .collect();
    if let Some(mut existing) = rude_intel::loader::load_chunks_from_cache(db_path) {
        let new_files: std::collections::HashSet<&str> =
            new_chunks.iter().map(|c| c.file.as_str()).collect();
        existing.retain(|c| !new_files.contains(c.file.as_str()));
        existing.extend(new_chunks);
        existing.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.lines.cmp(&b.lines)));
        existing
    } else {
        new_chunks
    }
}

fn prebuild_caches(
    db_path: &std::path::Path,
    new_entries: &[CodeChunkEntry],
    mir_edges: Option<&rude_intel::mir_edges::MirEdgeMap>,
    mir_edge_dir: &std::path::Path,
) {
    let chunks = merge_chunks_cache(db_path, new_entries);
    eprintln!("    [cache] {} chunks", chunks.len());

    rude_intel::loader::save_chunks_cache(db_path, &chunks);

    let incremental = mir_edges.map(|_| rude_intel::graph::IncrementalArgs {
        changed_crates: &[],
        mir_edge_dir,
    });
    rude_intel::graph::CallGraph::build_only(chunks, mir_edges, incremental, db_path)
        .save_background(db_path);
}
fn scan_files_fast(input_path: &std::path::Path, exclude: &[String]) -> Vec<PathBuf> {
    if let Ok(out) = std::process::Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(input_path).output()
    {
        if out.status.success() {
            let files: Vec<PathBuf> = String::from_utf8_lossy(&out.stdout).lines()
                .filter_map(|line| {
                    let p = input_path.join(line);
                    rude_db::is_code_ext(p.extension().and_then(|e| e.to_str()).unwrap_or("")).then_some(p)
                })
                .collect();
            if !files.is_empty() { return files; }
        }
    }
    scan_files(input_path, exclude, rude_db::is_code_ext)
}
