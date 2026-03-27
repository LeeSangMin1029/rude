use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use rude_db::file_index;
use rude_util::scan_files;
use rude_db::DbConfig;
use rude_util::is_interrupted;
use rude_db::StorageEngine;

use super::{ingest_mir, write_chunks, CodeChunkEntry};

const TEXT_ONLY_DIM: usize = 1;
const TEXT_ONLY_MODEL: &str = "text-only";

#[tracing::instrument(skip_all)]
fn prof() -> bool { std::env::var("RUDE_PROFILE").is_ok() }
macro_rules! prof {
    ($label:expr, $block:expr) => {{
        let _t = std::time::Instant::now();
        let _r = $block;
        if prof() { eprintln!("  [prof] {:30} {:>8.0}us", $label, _t.elapsed().as_secs_f64() * 1_000_000.0); }
        _r
    }};
}

pub fn run(input_path: PathBuf, exclude: &[String]) -> Result<()> {
    let db_path = crate::db().to_path_buf();
    use rude_util::get_file_mtime;

    rude_intel::parse::set_project_root(&input_path);

    println!("Indexing code: {}", input_path.display());
    println!("Database:      {}", db_path.display());

    let all_files = prof!("scan_files", scan_files_fast(&input_path, exclude));
    if !prof() { eprintln!("  scan: ({} files)", all_files.len()); }
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

    if code_files.is_empty() {
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

    let incremental_crates = prof!("mir_analysis", run_mir_analysis(&input_path, &mir_db, &code_files)?);
    prof!("sub_workspaces", run_sub_workspaces(&input_path, &mir_db, &code_files).ok());

    let mir_chunks = prof!("load_sqlite", rude_intel::mir_edges::MirEdgeMap::load_chunks_from_sqlite(
        &mir_db, to_crate_filter(&incremental_crates).as_deref(),
    ).context("failed to load MIR chunks")?);

    prof!("ingest_mir", ingest_mir(&mir_chunks, &db_path, &mut entries, &mut file_metadata_map, None)?);
    eprintln!("  chunk: {:.1}s ({} chunks)", t0.elapsed().as_secs_f64(), entries.len());

    println!("Symbols: {} (functions, structs, enums, ...)", entries.len());
    let inserted = prof!("write_chunks", write_chunks(&entries, &file_metadata_map, &mut file_idx, true)?);
    if !prof() { println!("\nInserted {inserted} chunks in 0.00s"); }

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
        println!("Use: rude context/blast/symbols/dupes {}", db_path.display());
        prof!("checkpoint", engine.checkpoint().ok());
        drop(engine);
        let db_bg = db_path.clone();
        let mir_bg = mir_out_dir.clone();
        let inc_bg = incremental_crates.clone();
        std::thread::spawn(move || prebuild_caches(&db_bg, &entries, &mir_bg, &inc_bg));
    }

    Ok(())
}


fn run_sub_workspaces(
    root: &std::path::Path, main_mir_db: &std::path::Path, code_files: &[&PathBuf],
) -> Result<()> {
    let sub_workspaces = find_sub_workspaces(root);
    let abs_root = rude_util::safe_canonicalize(root);
    for ws in &sub_workspaces {
        let abs_ws = rude_util::safe_canonicalize(ws);
        let ws_mir_db = abs_ws.join("target").join("mir-edges").join("mir.db");
        let has_changes = code_files.iter().any(|f| {
            let abs_f = if f.is_absolute() { f.to_path_buf() } else { abs_root.join(f) };
            abs_f.starts_with(&abs_ws)
        });
        if has_changes {
            eprintln!("  [mir] sub-workspace: {}", ws.display());
            let ws_args_dir = abs_ws.join("target").join("mir-edges").join("rustc-args");
            if ws_args_dir.exists() {
                let changed_ws: Vec<PathBuf> = code_files.iter().filter_map(|f| {
                    let abs_f = if f.is_absolute() { f.to_path_buf() } else { abs_root.join(f) };
                    abs_f.strip_prefix(&abs_ws).ok().map(|rel| abs_ws.join(rel))
                }).collect();
                let refs: Vec<&PathBuf> = changed_ws.iter().collect();
                let crates = rude_intel::mir_edges::detect_changed_crates(&abs_ws, &refs);
                if !crates.is_empty() {
                    let refs: Vec<&str> = crates.iter().map(|s| s.as_str()).collect();
                    rude_intel::mir_edges::clear_mir_db(&abs_ws, &refs).ok();
                    rude_intel::mir_edges::run_mir_direct(&abs_ws, None, &refs, true).ok();
                }
            } else {
                run_mir_cargo_wrapper(&abs_ws).ok();
            }
        }
        if ws_mir_db.exists() {
            rude_intel::mir_edges::merge_mir_db(main_mir_db, &ws_mir_db).ok();
        }
    }
    Ok(())
}



fn run_mir_cargo_wrapper(ws: &std::path::Path) -> Result<()> {
    let bin = rude_intel::mir_edges::find_mir_callgraph_bin(None)?;
    let out_dir = ws.join("target").join("mir-edges");
    std::fs::create_dir_all(&out_dir).ok();
    let abs_out = rude_util::safe_canonicalize(&out_dir);
    let abs_db = abs_out.join("mir.db");
    let abs_bin = rude_util::safe_canonicalize(&bin);
    let status = std::process::Command::new("cargo")
        .arg("check").arg("--tests")
        .env("RUSTUP_TOOLCHAIN", "nightly")
        .arg("--target-dir").arg(ws.join("target").join(rude_intel::mir_edges::mir_check_dir_name()))
        .current_dir(ws)
        .env("RUSTC_WRAPPER", &abs_bin)
        .env("MIR_CALLGRAPH_OUT", &abs_out)
        .env("MIR_CALLGRAPH_DB", &abs_db)
        .env("MIR_CALLGRAPH_JSON", "1")
        .status()
        .context("failed to run cargo check for sub-workspace")?;
    if !status.success() {
        eprintln!("  [mir] sub-workspace cargo check failed: {status}");
    }
    Ok(())
}

fn find_sub_workspaces(root: &std::path::Path) -> Vec<PathBuf> {
    let cache_file = root.join("target").join("mir-edges").join(".sub-workspaces");
    let toml_mtime = std::fs::metadata(root.join("Cargo.toml"))
        .and_then(|m| m.modified()).ok();
    let cache_mtime = std::fs::metadata(&cache_file)
        .and_then(|m| m.modified()).ok();
    if let (Some(t), Some(c)) = (toml_mtime, cache_mtime) {
        if c > t {
            if let Ok(content) = std::fs::read_to_string(&cache_file) {
                return content.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect();
            }
        }
    }
    let result = detect_sub_workspaces(root);
    let text: String = result.iter().filter_map(|p| p.to_str()).collect::<Vec<_>>().join("\n");
    let _ = std::fs::write(&cache_file, text);
    result
}

fn detect_sub_workspaces(root: &std::path::Path) -> Vec<PathBuf> {
    let meta_output = std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root).output().ok();
    let Some(out) = meta_output.filter(|o| o.status.success()) else { return Vec::new() };
    let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Vec::new() };
    let norm = |s: &str| s.replace('\\', "/").to_lowercase();
    let ws_root = meta.get("workspace_root").and_then(|v| v.as_str()).map(norm).unwrap_or_default();
    let members: std::collections::HashSet<String> = meta.get("packages")
        .and_then(|p| p.as_array())
        .map(|pkgs| pkgs.iter().filter_map(|p| {
            Some(norm(&PathBuf::from(p.get("manifest_path")?.as_str()?).parent()?.to_string_lossy()))
        }).collect())
        .unwrap_or_default();
    let git_output = std::process::Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "*/Cargo.toml"])
        .current_dir(root).output().ok();
    let Some(git_out) = git_output.filter(|o| o.status.success()) else { return Vec::new() };
    let abs_root = rude_util::safe_canonicalize(root);
    String::from_utf8_lossy(&git_out.stdout).lines()
        .filter_map(|line| {
            let parent = PathBuf::from(line).parent()?.to_path_buf();
            if parent.as_os_str().is_empty() { return None; }
            let abs_dir = rude_util::safe_canonicalize(&abs_root.join(&parent));
            let dir_norm = norm(&abs_dir.to_string_lossy());
            if members.contains(&dir_norm) || dir_norm == ws_root { return None; }
            Some(root.join(parent))
        })
        .collect()
}


fn to_crate_filter(crates: &[String]) -> Option<Vec<&str>> {
    if crates.is_empty() { None } else { Some(crates.iter().map(String::as_str).collect()) }
}

fn lang_summary(files: &[&PathBuf]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for f in files {
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
        *counts.entry(rude_util::lang_for_ext(ext)).or_default() += 1;
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
    let changed_crates = prof!("detect_changed_crates", rude_intel::mir_edges::detect_changed_crates(input_path, &rust_changed));
    if changed_crates.is_empty() { return Ok(Vec::new()); }

    let crate_refs: Vec<&str> = changed_crates.iter().map(|s| s.as_str()).collect();
    eprintln!("  [mir] incremental: {} crate(s) — {}", crate_refs.len(), crate_refs.join(", "));
    prof!("clear_mir_db", rude_intel::mir_edges::clear_mir_db(input_path, &crate_refs).ok());
    let rust_only = code_files.iter().all(|f| f.extension().and_then(|e| e.to_str()) == Some("rs"));
    prof!("run_mir_direct", rude_intel::mir_edges::run_mir_direct(input_path, None, &crate_refs, rust_only)
        .context("mir-callgraph incremental failed")?);
    Ok(changed_crates)
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
    _mir_edge_dir: &std::path::Path,
    incremental_crates: &[String],
) {
    if incremental_crates.is_empty() {
        let chunks = merge_chunks_cache(db_path, new_entries);
        eprintln!("    [cache] {} chunks", chunks.len());
        // 전체 인덱싱: monolithic + 크레이트별 양쪽 저장
        rude_intel::loader::save_chunks_cache(db_path, &chunks);
        rude_intel::loader::save_chunks_cache_for(db_path, &chunks, None);
        let graph = rude_intel::graph::CallGraph::build_only(chunks, None, None, db_path);
        let _ = graph.save(db_path);
    } else {
        // 증분: 변경 크레이트만 저장 (crate_name 기반)
        let new_chunks: Vec<rude_intel::parse::ParsedChunk> = new_entries.iter()
            .map(|e| e.chunk.clone()).collect();
        let changed: Vec<&str> = incremental_crates.iter().map(|s| s.as_str()).collect();
        rude_intel::loader::save_chunks_cache_for(db_path, &new_chunks, Some(&changed));
        eprintln!("    [cache] updated {} chunks for {} crate(s)", new_chunks.len(), changed.len());
        if let Ok(engine) = rude_db::StorageEngine::open(db_path) {
            let _ = engine.set_cache("graph", &[]);
        }
    }
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
                    rude_util::is_code_ext(p.extension().and_then(|e| e.to_str()).unwrap_or("")).then_some(p)
                })
                .collect();
            if !files.is_empty() { return files; }
        }
    }
    scan_files(input_path, exclude, rude_util::is_code_ext)
}
