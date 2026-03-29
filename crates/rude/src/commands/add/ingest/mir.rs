use std::collections::HashMap;
use std::path::Path;

use super::{
    parse_enum_variants, parse_field_types, parse_param_types, parse_return_type, CodeChunkEntry,
};

#[tracing::instrument(skip_all)]
pub(crate) fn ingest_mir(
    mir_chunks: &[rude_intel::mir_edges::MirChunk],
    db_path: &Path,
    entries: &mut Vec<CodeChunkEntry>,
    file_metadata_map: &mut HashMap<String, (u64, u64, Vec<u64>)>,
    changed_sources: Option<&std::collections::HashSet<String>>,
) -> Result<(), anyhow::Error> {
    use rude_intel::parse::normalize_path;
    use rude_util::{generate_id, normalize_source};

    let mut seen: HashMap<(&str, &str), usize> = HashMap::new();
    let mut deduped: Vec<&rude_intel::mir_edges::MirChunk> = Vec::new();
    for mc in mir_chunks {
        let key = (mc.name.as_str(), mc.file.as_str());
        if let Some(&idx) = seen.get(&key) {
            if deduped[idx].is_test && !mc.is_test {
                deduped[idx] = mc;
            }
        } else {
            seen.insert(key, deduped.len());
            deduped.push(mc);
        }
    }

    let mut by_file: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, mc) in deduped.iter().enumerate() {
        by_file.entry(mc.file.as_str()).or_default().push(i);
    }

    for (file_key, indices) in &by_file {
        let normalized_file = normalize_path(file_key);
        let db_parent = db_path.parent().unwrap_or(Path::new("."));
        let workspace = rude_util::safe_canonicalize(&db_parent);
        let file_path = workspace.join(file_key);
        let source = if file_path.exists() {
            normalize_source(&file_path)
        } else {
            normalize_source(Path::new(file_key))
        };
        if let Some(changed) = changed_sources {
            if !changed.contains(&source) {
                continue;
            }
        }
        let mtime = rude_util::get_file_mtime(&file_path).unwrap_or(0);
        let size = rude_db::file_index::get_file_size(&file_path).unwrap_or(0);
        let ext = std::path::Path::new(file_key)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let _lang = rude_util::lang_for_ext(ext);

        let file_lines: Option<Vec<String>> =
            if indices.iter().any(|&i| deduped[i].body.is_empty()) {
                std::fs::read_to_string(&file_path)
                    .ok()
                    .map(|content| content.lines().map(|l| l.to_owned()).collect())
            } else {
                None
            };

        let mut chunk_ids = Vec::with_capacity(indices.len());

        for &idx in indices {
            let mc = deduped[idx];
            let id = generate_id(&source, idx);
            chunk_ids.push(id);

            let mut parsed_chunk = mc.to_parsed();
            parsed_chunk.file = normalized_file.clone();
            parsed_chunk.chunk_index = idx;
            if parsed_chunk.crate_name.is_empty() {
                parsed_chunk.crate_name = crate_name_from_path(&file_path);
            }

            if parsed_chunk.text.is_empty() {
                if let Some(ref lines) = file_lines {
                    let start = mc.start_line.saturating_sub(1);
                    let end = mc.end_line.min(lines.len());
                    if start < end {
                        parsed_chunk.text = lines[start..end].join("\n");
                        parsed_chunk.compute_minhash();
                    }
                }
            }

            if parsed_chunk.signature.is_none() {
                parsed_chunk.signature = parsed_chunk.text.lines().next().and_then(|l| {
                    let s = l.split('{').next()?.trim();
                    (!s.is_empty()).then(|| s.to_owned())
                });
            }

            let chunk_lines_vec: Vec<&str> = parsed_chunk.text.lines().collect();
            parsed_chunk.param_types = parse_param_types(parsed_chunk.signature.as_deref());
            parsed_chunk.return_type = parse_return_type(parsed_chunk.signature.as_deref());
            if parsed_chunk.kind == "struct" {
                parsed_chunk.field_types = parse_field_types(&chunk_lines_vec);
            }
            if parsed_chunk.kind == "enum" {
                parsed_chunk.enum_variants = parse_enum_variants(&chunk_lines_vec);
            }

            if parsed_chunk.visibility.is_empty() {
                let fl = chunk_lines_vec.first().map(|l| l.trim()).unwrap_or("");
                parsed_chunk.visibility = ["pub(crate)", "pub(super)", "pub"]
                    .iter()
                    .find(|p| fl.starts_with(**p))
                    .map(|p| (*p).to_owned())
                    .unwrap_or_default();
            }

            entries.push(CodeChunkEntry {
                chunk: parsed_chunk,
            });
        }

        file_metadata_map.insert(source.clone(), (mtime, size, chunk_ids));
    }

    Ok(())
}

fn crate_name_from_path(file_path: &Path) -> String {
    let mut dir = file_path.parent();
    while let Some(d) = dir {
        let toml = d.join("Cargo.toml");
        if toml.exists() {
            if let Ok(content) = std::fs::read_to_string(&toml) {
                let mut in_package = false;
                for line in content.lines() {
                    let t = line.trim();
                    if t.starts_with('[') { in_package = t == "[package]"; continue; }
                    if in_package && t.starts_with("name") {
                        if let Some(name) = t.split('"').nth(1) {
                            return name.replace('-', "_");
                        }
                    }
                }
            }
            break;
        }
        dir = d.parent();
    }
    String::new()
}
