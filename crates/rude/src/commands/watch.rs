//! File watch mode — auto-updates DB on source changes via MIR.
//!
//! Event-driven: no sleep/timer. Uses notify + mpsc channel.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecursiveMode, Watcher};

use rude_db::file_index;
use rude_db::file_utils::{generate_id, normalize_source};
use rude_db::{Payload, PayloadValue};

/// Directories to skip.
const IGNORED_DIRS: &[&str] = &[".git", "target", "node_modules", "__pycache__"];

/// Run watch mode — blocks indefinitely, updating DB on file changes.
pub fn run(db_path: PathBuf, input_path: PathBuf) -> Result<()> {
    // Set project root for path normalization (absolute → relative).
    rude_intel::parse::set_project_root(&input_path);

    println!("[watch] Watching {} for changes...", input_path.display());
    println!("[watch] DB: {}", db_path.display());
    println!("[watch] Press Ctrl+C to stop\n");

    // Initial full build if DB doesn't exist
    if !db_path.exists() {
        eprintln!("[watch] No DB found, running initial add...");
        super::add::run(db_path.clone(), input_path.clone(), &[])?;
        eprintln!("[watch] Initial build complete\n");
    }

    let (tx, rx) = mpsc::channel::<PathBuf>();

    let sender = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };

        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ) {
            return;
        }

        for path in event.paths {
            if is_rust_source(&path) && !is_in_ignored_dir(&path) {
                let _ = sender.send(path);
            }
        }
    })
    .context("failed to create file watcher")?;

    watcher
        .watch(&input_path, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", input_path.display()))?;

    // Event loop — blocks on recv(), no polling
    let mut pending = HashSet::new();

    loop {
        // Block until first event (CPU 0% while waiting)
        match rx.recv() {
            Ok(path) => {
                pending.insert(path);
            }
            Err(_) => {
                eprintln!("[watch] Channel closed, stopping");
                break;
            }
        }

        // Drain any additional events that arrived in the same batch
        while let Ok(path) = rx.try_recv() {
            pending.insert(path);
        }

        // Process all pending changes
        let changed: Vec<PathBuf> = pending.drain().collect();
        process_changes(&changed, &db_path, &input_path);
    }

    Ok(())
}

/// Process a batch of changed files: detect crates -> MIR -> chunk -> graph.
fn process_changes(changed: &[PathBuf], db_path: &Path, input_path: &Path) {
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

    // 1. Detect changed crates
    let crates = rude_intel::mir_edges::detect_changed_crates(input_path, changed);
    if crates.is_empty() {
        eprintln!("[watch] no crate detected for changed files");
        return;
    }
    eprintln!("[watch] crate(s): {}", crates.join(", "));

    // 2. Run mir-callgraph for changed crates only
    let crate_refs: Vec<&str> = crates.iter().map(|s| s.as_str()).collect();
    let mir_edge_map =
        match rude_intel::mir_edges::run_mir_direct(input_path, None, &crate_refs, true) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[watch] mir-callgraph failed: {e}");
                return;
            }
        };

    // 3. Load MIR chunks — prefer sqlite, fallback to JSONL
    let mir_out_dir = input_path.join("target").join("mir-edges");
    let mir_db = rude_intel::mir_edges::mir_db_path(input_path);
    let mir_chunks = if mir_db.exists() {
        match rude_intel::mir_edges::MirEdgeMap::load_chunks_from_sqlite(&mir_db, None) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[watch] failed to load MIR chunks from sqlite: {e}");
                return;
            }
        }
    } else {
        match rude_intel::mir_edges::MirEdgeMap::load_chunks_from_sqlite(&mir_db, None) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[watch] failed to load MIR chunks: {e}");
                return;
            }
        }
    };

    // 4. Create chunk entries from changed files only
    let changed_sources: HashSet<String> = changed.iter().map(|f| normalize_source(f)).collect();

    let mut entries = Vec::new();
    let mut file_metadata_map = HashMap::new();
    if let Err(e) = super::ingest::chunk_from_mir(
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

    // 5. Build reverse index for called_by resolution
    let reverse_index = super::ingest::build_called_by_index(&entries);

    // 6. Write to DB
    if let Err(e) = update_db(db_path, &entries, &reverse_index, &file_metadata_map) {
        eprintln!("[watch] DB update failed: {e}");
        return;
    }

    // 7. Rebuild graph cache (incremental: only re-resolve changed crates)
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

/// Write chunk entries to DB.
fn update_db(
    db_path: &Path,
    entries: &[super::ingest::CodeChunkEntry],
    reverse_index: &HashMap<String, Vec<String>>,
    file_metadata_map: &HashMap<String, (u64, u64, Vec<u64>)>,
) -> Result<()> {
    let mut engine = rude_db::StorageEngine::open_exclusive(db_path)
        .context("failed to open DB for writing")?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Build per-file chunk count for accurate chunk_total.
    let chunk_total_map: HashMap<&str, usize> = {
        let mut m: HashMap<&str, usize> = HashMap::new();
        for entry in entries {
            *m.entry(entry.source.as_str()).or_default() += 1;
        }
        m
    };

    for entry in entries {
        let chunk = &entry.chunk;
        let id = generate_id(&entry.source, chunk.chunk_index);

        let called_by_refs = super::ingest::lookup_called_by(reverse_index, &chunk.name);
        let called_by_strings: Vec<String> =
            called_by_refs.iter().map(|s| (*s).to_owned()).collect();

        let is_test = rude_intel::graph::is_test_path(&entry.source)
            || chunk.name.starts_with("test_");

        // Build tags
        let mut tags = Vec::with_capacity(4 + called_by_refs.len());
        tags.push(format!("kind:{}", chunk.kind.as_str()));
        tags.push(format!("lang:{}", entry.lang));
        tags.push(format!(
            "role:{}",
            if is_test { "test" } else { "prod" }
        ));
        if !chunk.visibility.is_empty() {
            tags.push(format!("vis:{}", chunk.visibility));
        }
        for caller in &called_by_refs {
            tags.push(format!("caller:{caller}"));
        }

        let mut custom = chunk.to_custom_fields(&called_by_strings);
        custom.insert(
            "title".into(),
            PayloadValue::String(chunk.name.clone()),
        );

        let payload = Payload {
            source: entry.source.clone(),
            tags,
            created_at: now,
            source_modified_at: entry.mtime,
            chunk_index: chunk.chunk_index as u32,
            chunk_total: chunk_total_map.get(entry.source.as_str()).copied().unwrap_or(1) as u32,
            custom,
        };

        let embed_text = chunk.to_embed_text(&entry.file_path_str, &called_by_strings);

        engine.insert(id, &payload, &embed_text)?;
    }

    engine.checkpoint()?;

    // Update file index
    let mut file_idx = file_index::load_file_index(db_path)?;
    for (path, (mtime, size, chunk_ids)) in file_metadata_map {
        file_idx.update_file(path.clone(), *mtime, *size, chunk_ids.clone());
    }
    file_index::save_file_index(db_path, &file_idx)?;

    Ok(())
}

/// Rebuild graph.bin cache from current chunks + MIR edges.
fn rebuild_graph_cache(
    db_path: &Path,
    mir_out_dir: &Path,
    mir_edge_map: &rude_intel::mir_edges::MirEdgeMap,
    changed_crates: &[String],
) -> Result<()> {
    let chunks = rude_intel::loader::load_chunks(db_path)?;

    let incremental = rude_intel::graph::IncrementalArgs {
        changed_crates,
        mir_edge_dir: mir_out_dir,
    };
    let graph = rude_intel::graph::CallGraph::rebuild(
        db_path, &chunks, Some(mir_edge_map), Some(incremental),
    )?;
    eprintln!(
        "[watch] graph: {} nodes, {} edges",
        graph.len(),
        graph.callees.iter().map(Vec::len).sum::<usize>()
    );

    // Also update chunks.bin cache
    let cache_path = db_path.join("cache").join("chunks.bin");
    rude_intel::loader::save_chunks_cache(&cache_path, &chunks);

    Ok(())
}

fn is_rust_source(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "rs")
}

/// Check if a path is inside an ignored directory.
fn is_in_ignored_dir(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| IGNORED_DIRS.contains(&s))
    })
}
