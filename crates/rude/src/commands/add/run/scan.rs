use std::collections::HashMap;
use std::path::PathBuf;

use rude_util::scan_files;

use crate::commands::add::CodeChunkEntry;

#[tracing::instrument(skip_all)]
pub fn is_profiling() -> bool { std::env::var("RUDE_PROFILE").is_ok() }
macro_rules! prof {
    ($label:expr, $block:expr) => {{
        let _t = std::time::Instant::now();
        let _r = $block;
        if $crate::commands::add::run::scan::is_profiling() { eprintln!("  [prof] {:30} {:>8.0}us", $label, _t.elapsed().as_secs_f64() * 1_000_000.0); }
        _r
    }};
}
pub(crate) use prof;

pub fn scan_files_fast(input_path: &std::path::Path, exclude: &[String]) -> Vec<PathBuf> {
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

pub fn lang_summary(files: &[&PathBuf]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for f in files {
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
        *counts.entry(rude_util::lang_for_ext(ext)).or_default() += 1;
    }
    let mut pairs: Vec<_> = counts.iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(a.1));
    pairs.iter().map(|(l, n)| format!("{l}:{n}")).collect::<Vec<_>>().join(", ")
}

pub fn merge_chunks_cache(
    new_entries: &[CodeChunkEntry],
) -> Vec<rude_intel::parse::ParsedChunk> {
    let new_chunks: Vec<rude_intel::parse::ParsedChunk> = new_entries.iter()
        .map(|e| e.chunk.clone())
        .collect();
    if let Some(mut existing) = rude_intel::loader::load_chunks_from_cache() {
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

pub fn prebuild_caches(
    new_entries: &[CodeChunkEntry],
    incremental_crates: &[String],
) {
    if incremental_crates.is_empty() {
        let chunks = merge_chunks_cache(new_entries);
        tracing::debug!("[cache] {} chunks", chunks.len());
        rude_intel::loader::save_chunks_cache(&chunks);
        rude_intel::loader::save_chunks_cache_for(&chunks, None);
        let graph = rude_intel::graph::CallGraph::build_only(chunks, None, None);
        let _ = graph.save();
    } else {
        let new_chunks: Vec<rude_intel::parse::ParsedChunk> = new_entries.iter()
            .map(|e| e.chunk.clone()).collect();
        let changed: Vec<&str> = incremental_crates.iter().map(|s| s.as_str()).collect();
        rude_intel::loader::save_chunks_cache_for(&new_chunks, Some(&changed));
        tracing::debug!("[cache] updated {} chunks for {} crate(s)", new_chunks.len(), changed.len());
        if let Ok(engine) = rude_db::StorageEngine::open(crate::db()) {
            let _ = engine.set_cache("graph", &[]);
        }
    }
}
