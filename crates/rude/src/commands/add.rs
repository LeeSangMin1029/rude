//! Code-specific add/update command.
//!
//! Chunks code files via tree-sitter, stores text + payload only.
//! No embedding or index building.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use rude_db::file_index;
use rude_db::file_utils::scan_files;
use rude_db::DbConfig;
use rude_db::is_interrupted;
use rude_db::StorageEngine;
use super::ingest::CodeChunkEntry;

/// Placeholder dimension for text-only storage (no real vectors).
const TEXT_ONLY_DIM: usize = 1;
/// Model name stored in config for later embed to detect.
const TEXT_ONLY_MODEL: &str = "text-only";


// ── Public entry points ──────────────────────────────────────────────────

/// Run the rude add command (auto-incremental: only re-processes changed files).
pub fn run(db_path: PathBuf, input_path: PathBuf, exclude: &[String]) -> Result<()> {
    use rude_db::file_utils::get_file_mtime;

    // Set project root for path normalization (absolute → relative).
    rude_intel::parse::set_project_root(&input_path);

    println!("Indexing code: {}", input_path.display());
    println!("Database:      {}", db_path.display());

    // Scan for code files — prefer `git ls-files` (instant) over walkdir (slow fs walk).
    let t_scan = std::time::Instant::now();
    let all_files = scan_files_fast(&input_path, exclude);
    eprintln!("  scan: {:.1}ms ({} files)", t_scan.elapsed().as_secs_f64() * 1000.0, all_files.len());
    if all_files.is_empty() {
        anyhow::bail!(
            "No supported code files found in {}",
            input_path.display()
        );
    }

    // current_sources: must use normalize_source (canonicalize) to match file_index keys.
    let current_sources: std::collections::HashSet<String> = all_files
        .iter()
        .map(|f| rude_db::file_utils::normalize_source(f))
        .collect();

    // Filter to changed files only (mtime check).
    // Use normalize_source for file_idx lookup (keys are absolute paths).
    let file_idx = file_index::load_file_index(&db_path)?;
    let code_files: Vec<_> = all_files
        .iter()
        .filter(|f| {
            let source = rude_db::file_utils::normalize_source(f);
            match file_idx.get_file(&source) {
                Some(entry) => get_file_mtime(*f).is_none_or(|m| m != entry.mtime),
                None => true,
            }
        })
        .collect();

    // source_cache: full canonicalize only for code_files (changed files, typically 1-5).
    let source_cache: HashMap<std::path::PathBuf, String> = code_files
        .iter()
        .map(|f| ((*f).clone(), rude_db::file_utils::normalize_source(f)))
        .collect();

    if code_files.is_empty() {
        println!("No files changed. Nothing to update.");
        return Ok(());
    }

    // Collect language stats
    let mut lang_counts: HashMap<&str, usize> = HashMap::new();
    for f in &code_files {
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = rude_db::lang_for_ext(ext);
        *lang_counts.entry(lang).or_default() += 1;
    }
    let mut lang_summary: Vec<_> = lang_counts.iter().collect();
    lang_summary.sort_by(|a, b| b.1.cmp(a.1));
    let summary: Vec<String> = lang_summary.iter().map(|(l, n)| format!("{l}:{n}")).collect();
    println!("Files: {} ({})", code_files.len(), summary.join(", "));

    // Open/create database (text-only, dim=1 placeholder)
    let mut engine = if db_path.exists() {
        StorageEngine::open_exclusive(&db_path)
            .with_context(|| format!("Failed to open database at {}", db_path.display()))?
    } else {
        println!("New database: {} (dim={TEXT_ONLY_DIM})", db_path.display());
        // Clear stale edge/chunk JSONL + cargo fingerprints for local crates.
        // Keep target/mir-check/debug/deps/ (compiled deps) and incremental/ (rustc cache).
        let mir_edges = input_path.join("target").join("mir-edges");
        if mir_edges.exists() { let _ = std::fs::remove_dir_all(&mir_edges); }
        // Remove cargo fingerprint dir to force RUSTC_WRAPPER re-invocation.
        // Keeps deps/ and incremental/ intact for fast recompilation.
        let fingerprint = input_path.join("target/mir-check/debug/.fingerprint");
        if fingerprint.exists() { let _ = std::fs::remove_dir_all(&fingerprint); }
        let engine = StorageEngine::open_exclusive(&db_path)
            .with_context(|| format!("Failed to create database at {}", db_path.display()))?;
        DbConfig {
            code: true,
            embedded: false,
            embed_model: Some(TEXT_ONLY_MODEL.to_owned()),
            ..DbConfig::default()
        }.save(&db_path)?;
        engine
    };

    // Update config
    if let Ok(mut config) = DbConfig::load(&db_path) {
        config.code = true;
        config.embedded = false;
        if let Ok(canonical) = input_path.canonicalize() {
            let path_str = canonical.to_string_lossy();
            config.input_path = Some(rude_db::strip_unc_prefix(&path_str).to_owned());
        }
        let _ = config.save(&db_path);
    }

    // === Pass 1: Extract chunks via MIR (no daemon needed) ===
    let t0 = std::time::Instant::now();
    let mut entries: Vec<CodeChunkEntry> = Vec::new();
    let mut file_metadata_map: HashMap<String, (u64, u64, Vec<u64>)> = HashMap::new();

    // Run mir-callgraph — only for crates containing changed files
    let mir_out_dir = input_path.join("target").join("mir-edges");
    std::fs::create_dir_all(&mir_out_dir).ok();

    let has_cached_edges = mir_out_dir.exists()
        && std::fs::read_dir(&mir_out_dir).is_ok_and(|mut d| d.any(|e| {
            e.is_ok_and(|e| e.path().to_string_lossy().ends_with(".edges.jsonl"))
        }));

    let mut incremental_crates: Vec<String> = Vec::new();
    if has_cached_edges {
        // Incremental: re-analyze crates with changed OR new Rust files
        let rust_changed: Vec<_> = code_files.iter()
            .filter(|f| f.extension().and_then(|e| e.to_str()) == Some("rs"))
            .collect();
        let mut changed_crates = rude_intel::mir_edges::detect_changed_crates(&input_path, &rust_changed);

        // Integrity check: add crates whose edge files are missing
        let missing = rude_intel::mir_edges::detect_missing_edge_crates(&input_path);
        if !missing.is_empty() {
            eprintln!("  [mir] missing edge files for: {}", missing.join(", "));
            for m in missing {
                if !changed_crates.contains(&m) {
                    changed_crates.push(m);
                }
            }
        }

        if !changed_crates.is_empty() {
            let crate_refs: Vec<&str> = changed_crates.iter().map(|s| s.as_str()).collect();
            eprintln!("  [mir] incremental: {} crate(s) — {}", crate_refs.len(), crate_refs.join(", "));
            // Skip py/ts extractors when only .rs files changed (~0.3s saving)
            let rust_only = code_files.iter().all(|f| {
                f.extension().and_then(|e| e.to_str()) == Some("rs")
            });
            rude_intel::mir_edges::run_mir_direct(&input_path, None, &crate_refs, rust_only)
                .context("mir-callgraph incremental failed")?;
            incremental_crates = changed_crates;
        }
    } else {
        // Initial: analyze entire workspace
        rude_intel::mir_edges::run_mir_callgraph(&input_path, None)
            .context("mir-callgraph failed — ensure nightly rustc and mir-callgraph are installed")?;
    }

    // Load MIR chunks — only for changed crates (skip 94MB full parse on incremental)
    let mir_chunks = if incremental_crates.is_empty() {
        rude_intel::mir_edges::load_all_mir_chunks(&mir_out_dir)
    } else {
        let refs: Vec<&str> = incremental_crates.iter().map(|s| s.as_str()).collect();
        rude_intel::mir_edges::load_mir_chunks_filtered(&mir_out_dir, Some(&refs))
    }.context("failed to load MIR chunks")?;

    let changed_sources: std::collections::HashSet<String> = code_files.iter()
        .filter_map(|f| source_cache.get(*f).cloned())
        .collect();

    super::ingest::chunk_from_mir(&mir_chunks, &db_path, &mut entries, &mut file_metadata_map, Some(&changed_sources))?;

    eprintln!("  chunk: {:.1}s ({} chunks)", t0.elapsed().as_secs_f64(), entries.len());

    // === Build called_by + direct bulk write (zero-copy path) ===
    println!("Symbols: {} (functions, structs, enums, ...)", entries.len());
    let inserted = direct_bulk_write(&db_path, &entries, &mut engine, &file_metadata_map)?;


    // === Record mtime for scanned files with 0 chunks (avoid re-detection) ===
    let mut file_idx = file_index::load_file_index(&db_path)?;
    for f in &code_files {
        let source = source_cache.get(*f).cloned().unwrap_or_default();
        if !file_metadata_map.contains_key(&source) {
            if let Some(mtime) = get_file_mtime(*f) {
                let size = file_index::get_file_size(*f).unwrap_or(0);
                let existing_chunk_ids = file_idx
                    .get_file(&source)
                    .map(|e| e.chunk_ids.clone())
                    .unwrap_or_default();
                file_idx.update_file(source, mtime, size, existing_chunk_ids);
            }
        }
    }
    file_index::save_file_index(&db_path, &file_idx)?;

    // === Remove chunks from deleted files ===
    let deleted: Vec<String> = file_idx.files.keys()
        .filter(|p| !current_sources.contains(p.as_str()))
        .cloned()
        .collect();
    if !deleted.is_empty() {
        let mut del_count = 0usize;
        for path in &deleted {
            if let Some(entry) = file_idx.files.remove(path) {
                for id in &entry.chunk_ids {
                    let _ = engine.remove(*id);
                    del_count += 1;
                }
            }
        }
        if del_count > 0 {
            engine.checkpoint().ok();
            file_index::save_file_index(&db_path, &file_idx)?;
            eprintln!("Removed {del_count} chunks from {n} deleted file(s)", n = deleted.len());
        }
    }

    if is_interrupted() {
        println!();
        println!("Operation interrupted. Partial data may have been inserted.");
        return Ok(());
    }

    let has_changes = inserted > 0 || !deleted.is_empty();

    if !has_changes {
        println!("No changes. Database is up to date.");
    } else {
        println!();
        println!("Done! Code DB ready: {}", db_path.display());
        println!("Use: rude context/blast/symbols/dupes {}", db_path.display());

        // Checkpoint + release exclusive lock before rebuilding caches.
        engine.checkpoint().ok();
        drop(engine);

        // Load only changed crates' MIR edges (not all 22MB).
        let mir_edges = if mir_out_dir.exists() && !incremental_crates.is_empty() {
            let refs: Vec<&str> = incremental_crates.iter().map(|s| s.as_str()).collect();
            rude_intel::mir_edges::MirEdgeMap::from_dir_filtered(&mir_out_dir, Some(&refs)).ok()
        } else if mir_out_dir.exists() {
            rude_intel::mir_edges::MirEdgeMap::from_dir(&mir_out_dir).ok()
        } else {
            None
        };

        prebuild_caches(&db_path, &entries, &current_sources, &all_files, mir_edges.as_ref(), &mir_out_dir);
    }

    Ok(())
}

/// Rebuild chunks.bin + graph.bin caches.
///
/// SQLite is the source of truth. Load all chunks from DB, rebuild caches.
/// No merge logic needed — sqlite already has the correct state after insert.
fn prebuild_caches(
    db_path: &std::path::Path,
    new_entries: &[CodeChunkEntry],
    _current_sources: &std::collections::HashSet<String>,
    _all_files: &[PathBuf],
    mir_edges: Option<&rude_intel::mir_edges::MirEdgeMap>,
    mir_edge_dir: &std::path::Path,
) {
    use rude_intel::parse::ParsedChunk;

    let cache = rude_intel::loader::cache_path(db_path);
    if let Some(parent) = cache.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Fast path: load chunks.bin cache + merge new entries (avoids 37K sqlite load).
    // Fallback: load from sqlite if no cache.
    let chunks: Vec<ParsedChunk> = {
        let new_chunks: Vec<ParsedChunk> = new_entries.iter()
            .map(|e| ParsedChunk::from_code_chunk(&e.chunk, &e.file_path_str, e.chunk.imports.clone()))
            .collect();
        let new_files: std::collections::HashSet<String> = new_chunks.iter()
            .map(|c| c.file.clone()).collect();

        if let Some(mut existing) = rude_intel::loader::load_chunks_from_cache(db_path) {
            // Remove chunks from changed files, add new ones
            existing.retain(|c| !new_files.contains(c.file.as_str()));
            existing.extend(new_chunks);
            existing.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.lines.cmp(&b.lines)));
            existing
        } else {
            // No cache — full load from sqlite
            rude_intel::loader::load_chunks_from_db(db_path).unwrap_or(new_chunks)
        }
    };
    eprintln!("    [cache] {} chunks", chunks.len());

    // Save chunks.bin
    rude_intel::loader::save_chunks_cache(&cache, &chunks);
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&cache) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }

    // Build graph, then save asynchronously in background thread.
    // Encoding happens synchronously (current thread), only file I/O is async.
    // This shaves ~30-50ms off the critical path for large graph.bin (~11MB).
    let incremental = mir_edges.map(|_| rude_intel::graph::IncrementalArgs {
        changed_crates: &[],  // empty → staleness-based detection
        mir_edge_dir,
    });
    let graph = rude_intel::graph::CallGraph::build_only(
        &chunks, mir_edges, incremental, db_path,
    );
    graph.save_background(db_path);
}

/// Zero-copy ingest: CodeChunkEntry → Payload bincode → disk.
///
/// Skips IngestRecord and make_payload intermediates. Builds Payload inline
/// from entry references, encodes directly to contiguous buffer, single I/O.
fn direct_bulk_write(
    db_path: &std::path::Path,
    entries: &[super::ingest::CodeChunkEntry],
    engine: &mut StorageEngine,
    file_metadata_map: &HashMap<String, (u64, u64, Vec<u64>)>,
) -> Result<u64> {
    use rude_db::file_utils::generate_id;
    use rude_db::{Payload, PayloadValue};

    // Remove stale chunks for files being re-added.
    let file_index_data = file_index::load_file_index(db_path)?;
    for (path, (_, _, new_ids)) in file_metadata_map {
        if let Some(existing) = file_index_data.get_file(path) {
            let new_id_set: std::collections::HashSet<u64> = new_ids.iter().copied().collect();
            for &old_id in &existing.chunk_ids {
                if !new_id_set.contains(&old_id) {
                    let _ = engine.remove(old_id);
                }
            }
        }
    }

    let start = std::time::Instant::now();

    // Build called_by reverse index (needed for embed_text + tags).
    let reverse_index = super::ingest::build_called_by_index(entries);
    let chunk_total_map: HashMap<&str, usize> = {
        let mut m: HashMap<&str, usize> = HashMap::new();
        for entry in entries {
            *m.entry(&entry.source).or_default() += 1;
        }
        m
    };

    // Encode payloads + texts directly into contiguous buffers (zero-copy path).
    // No IngestRecord or intermediate Payload allocation per record.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Parallel encode: each entry independently produces (id, payload, text).
    use rayon::prelude::*;
    let encoded: Vec<(u64, Payload, String)> = entries
        .par_iter()
        .map(|entry| {
            let chunk = &entry.chunk;
            let id = generate_id(&entry.source, chunk.chunk_index);
            let chunk_total = chunk_total_map.get(entry.source.as_str()).copied().unwrap_or(1);
            let called_by_refs = super::ingest::lookup_called_by(&reverse_index, &chunk.name);


            let mut tags = Vec::with_capacity(4 + called_by_refs.len());
            tags.push(format!("kind:{}", chunk.kind.as_str()));
            tags.push(format!("lang:{}", entry.lang));
            if !chunk.visibility.is_empty() {
                tags.push(format!("vis:{}", chunk.visibility));
            }
            for caller in &called_by_refs {
                tags.push(format!("caller:{caller}"));
            }

            let called_by_strings: Vec<String> = called_by_refs.iter().map(|s| (*s).to_owned()).collect();
            let custom = chunk.to_custom_fields(&called_by_strings);

            let mut custom_with_title = custom;
            custom_with_title.insert("title".into(), PayloadValue::String(chunk.name.clone()));

            let payload = Payload {
                source: entry.source.clone(),
                tags,
                created_at: now,
                source_modified_at: entry.mtime,
                chunk_index: chunk.chunk_index as u32,
                chunk_total: chunk_total as u32,
                custom: custom_with_title,
            };

            let embed_text = chunk.to_embed_text(&entry.file_path_str, &called_by_strings);

            (id, payload, embed_text)
        })
        .collect();

    // Build batch for insert_batch: Vec<(u64, Payload, &str)>.
    let batch: Vec<(u64, Payload, &str)> = encoded
        .iter()
        .map(|(id, payload, text)| (*id, payload.clone(), text.as_str()))
        .collect();

    engine.insert_batch(&batch)
        .context("Failed to bulk load")?;

    // Update file index.
    let mut file_idx = file_index::load_file_index(db_path)?;
    for (path, (mtime, size, chunk_ids)) in file_metadata_map {
        file_idx.update_file(path.to_string(), *mtime, *size, chunk_ids.clone());
    }
    file_index::save_file_index(db_path, &file_idx)?;

    let inserted = entries.len() as u64;
    println!("\nInserted {inserted} chunks in {:.2}s", start.elapsed().as_secs_f64());

    Ok(inserted)
}


/// Fast file scan: `git ls-files` (instant from index) with walkdir fallback.
fn scan_files_fast(input_path: &std::path::Path, exclude: &[String]) -> Vec<PathBuf> {
    if let Ok(output) = std::process::Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(input_path)
        .output()
    {
        if output.status.success() {
            let files: Vec<PathBuf> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let path = input_path.join(line);
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    if rude_db::is_code_ext(ext) {
                        Some(path)
                    } else {
                        None
                    }
                })
                .collect();
            if !files.is_empty() {
                return files;
            }
        }
    }
    // Fallback to walkdir.
    scan_files(input_path, exclude, rude_db::is_code_ext)
}
