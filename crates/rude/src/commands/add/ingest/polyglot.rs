use std::collections::HashMap;
use std::path::Path;
use anyhow::Result;
use rude_intel::mir_edges::polyglot::{ExtCallEdge, ExtChunk};
use rude_intel::parse::{normalize_path, ParsedChunk};
use super::CodeChunkEntry;

pub(crate) fn ingest_polyglot(
    edges: &[ExtCallEdge],
    chunks: &[ExtChunk],
    project_dir: &Path,
    entries: &mut Vec<CodeChunkEntry>,
    file_metadata_map: &mut HashMap<String, (u64, u64, Vec<u64>)>,
) -> Result<()> {
    let project_name = detect_project_name(project_dir);
    let mut chunk_map: HashMap<String, usize> = HashMap::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let file = normalize_path(&chunk.file);
        let source = rude_util::normalize_source(Path::new(&chunk.file));
        let id = rude_util::generate_id(&source, i);
        chunk_map.insert(chunk.name.clone(), entries.len());
        let file_path = if Path::new(&chunk.file).is_absolute() {
            std::path::PathBuf::from(&chunk.file)
        } else {
            project_dir.join(&chunk.file)
        };
        let text = if chunk.text.is_empty() {
            read_chunk_text(&file_path, chunk.start, chunk.end)
        } else {
            chunk.text.clone()
        };
        let crate_name = if !chunk.crate_name.is_empty() {
            chunk.crate_name.clone()
        } else {
            extract_module_from_symbol(&chunk.name).unwrap_or_else(|| project_name.clone())
        };
        let display_name = rude_util::display_symbol_name(&chunk.name);
        let mut parsed = ParsedChunk {
            name: chunk.name.clone(),
            display_name,
            file: file.clone(),
            kind: chunk.kind.clone(),
            lines: Some((chunk.start, chunk.end)),
            signature: if chunk.signature.is_empty() { None } else { Some(chunk.signature.clone()) },
            crate_name,
            calls: Vec::new(),
            text,
            chunk_index: i,
            ..Default::default()
        };
        parsed.compute_minhash();
        entries.push(CodeChunkEntry { chunk: parsed });
        let mtime = rude_util::get_file_mtime(&file_path).unwrap_or(0);
        let size = rude_db::file_index::get_file_size(&file_path).unwrap_or(0);
        file_metadata_map.entry(source).or_insert_with(|| (mtime, size, Vec::new())).2.push(id);
    }
    for edge in edges {
        if let Some(&caller_idx) = chunk_map.get(&edge.caller) {
            entries[caller_idx].chunk.calls.push(edge.callee.clone());
        }
    }
    Ok(())
}

fn read_chunk_text(file_path: &Path, start: usize, end: usize) -> String {
    if start == 0 && end == 0 { return String::new(); }
    let Ok(content) = std::fs::read_to_string(file_path) else { return String::new() };
    let lines: Vec<&str> = content.lines().collect();
    let s = start.saturating_sub(1);
    let e = end.min(lines.len());
    if s < e { lines[s..e].join("\n") } else { String::new() }
}

fn detect_project_name(project_dir: &Path) -> String {
    if let Ok(content) = std::fs::read_to_string(project_dir.join("go.mod")) {
        for line in content.lines() {
            if let Some(module) = line.strip_prefix("module ") {
                return module.trim().to_string();
            }
        }
    }
    if let Ok(content) = std::fs::read_to_string(project_dir.join("package.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                return name.to_string();
            }
        }
    }
    project_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "unknown".to_string())
}

fn extract_module_from_symbol(name: &str) -> Option<String> {
    let last_dot = name.rfind('.')?;
    Some(name[..last_dot].to_string())
}
