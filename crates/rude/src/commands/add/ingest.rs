use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rude_db::file_index::FileIndex;
use rude_intel::parse::ParsedChunk;

pub(crate) struct CodeChunkEntry {
    pub chunk: ParsedChunk,
    pub source: String,
    pub mtime: u64,
    pub lang: &'static str,
}

pub(crate) fn build_callers(entries: &[CodeChunkEntry]) -> HashMap<String, Vec<String>> {
    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();

    for entry in entries {
        let caller = &entry.chunk.name;
        for call in &entry.chunk.calls {
            let bare = call
                .rsplit_once("::")
                .map(|(_, name)| name)
                .or_else(|| call.rsplit_once('.').map(|(_, name)| name))
                .unwrap_or(call);

            reverse
                .entry(bare.to_owned())
                .or_default()
                .push(caller.clone());
        }
    }

    for callers in reverse.values_mut() {
        callers.sort();
        callers.dedup();
    }

    reverse
}

// Checks both the full qualified name and the bare (last segment) name
// to handle calls recorded as either "crate::mod::fn" or just "fn".
fn find_callers<'a>(
    reverse: &'a HashMap<String, Vec<String>>,
    chunk_name: &str,
) -> Vec<&'a str> {
    let bare = chunk_name
        .rsplit_once("::")
        .map(|(_, name)| name)
        .unwrap_or(chunk_name);

    let mut result: Vec<&str> = Vec::new();

    if let Some(callers) = reverse.get(bare) {
        for c in callers {
            if c != chunk_name {
                result.push(c.as_str());
            }
        }
    }

    if bare != chunk_name
        && let Some(callers) = reverse.get(chunk_name)
    {
        for c in callers {
            if c != chunk_name && !result.contains(&c.as_str()) {
                result.push(c.as_str());
            }
        }
    }

    result.sort();
    result
}

pub(crate) fn build_payload(
    entry: &CodeChunkEntry,
    now: u64,
    chunk_total: usize,
    reverse: &HashMap<String, Vec<String>>,
    include_role_tag: bool,
) -> (u64, rude_db::Payload, String) {
    use rude_db::file_utils::generate_id;
    use rude_db::PayloadValue;

    let chunk = &entry.chunk;
    let id = generate_id(&entry.source, chunk.chunk_index);

    let called_by_refs = find_callers(reverse, &chunk.name);
    let called_by_strings: Vec<String> = called_by_refs.iter().map(|s| (*s).to_owned()).collect();

    let is_test = rude_intel::graph::is_test_path(&entry.source) || chunk.name.starts_with("test_");

    let mut tags = Vec::with_capacity(5 + called_by_refs.len());
    tags.push(format!("kind:{}", chunk.kind));
    tags.push(format!("lang:{}", entry.lang));
    if include_role_tag {
        tags.push(format!("role:{}", if is_test { "test" } else { "prod" }));
    }
    if !chunk.visibility.is_empty() {
        tags.push(format!("vis:{}", chunk.visibility));
    }
    for caller in &called_by_refs {
        tags.push(format!("caller:{caller}"));
    }

    let mut custom = chunk.to_custom_fields(&called_by_strings);
    custom.insert("title".into(), PayloadValue::String(chunk.name.clone()));

    let payload = rude_db::Payload {
        source: entry.source.clone(),
        tags,
        created_at: now,
        source_modified_at: entry.mtime,
        chunk_index: chunk.chunk_index as u32,
        chunk_total: chunk_total as u32,
        custom,
    };

    let embed_text = chunk.text.clone();

    (id, payload, embed_text)
}

fn parse_param_types(signature: Option<&str>) -> Vec<(String, String)> {
    signature
        .and_then(|sig| {
            let paren_start = sig.find('(')?;
            let paren_end = sig.rfind(')')?;
            if paren_start >= paren_end {
                return None;
            }
            let params_str = &sig[paren_start + 1..paren_end];
            let pairs: Vec<(String, String)> = params_str
                .split(',')
                .filter_map(|p| {
                    let p = p.trim();
                    if p == "self" || p == "&self" || p == "&mut self" || p.is_empty() {
                        return None;
                    }
                    let (name, ty) = p.split_once(':')?;
                    Some((name.trim().to_owned(), ty.trim().to_owned()))
                })
                .collect();
            Some(pairs)
        })
        .unwrap_or_default()
}

fn parse_return_type(signature: Option<&str>) -> Option<String> {
    signature.and_then(|sig| {
        let after_arrow = sig.split("->").nth(1)?;
        let rt = after_arrow.trim().trim_end_matches('{').trim();
        if rt.is_empty() { None } else { Some(rt.to_owned()) }
    })
}

fn parse_field_types(chunk_lines: &[&str]) -> Vec<(String, String)> {
    if chunk_lines.len() <= 1 {
        return Vec::new();
    }
    chunk_lines[1..]
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim().trim_end_matches(',');
            if trimmed.starts_with("//") || trimmed.is_empty() || trimmed == "}" {
                return None;
            }
            let stripped = trimmed
                .strip_prefix("pub(crate) ")
                .or_else(|| trimmed.strip_prefix("pub(super) "))
                .or_else(|| trimmed.strip_prefix("pub "))
                .unwrap_or(trimmed);
            let (name, ty) = stripped.split_once(':')?;
            Some((name.trim().to_owned(), ty.trim().to_owned()))
        })
        .collect()
}

fn parse_enum_variants(chunk_lines: &[&str]) -> Vec<String> {
    if chunk_lines.len() <= 1 {
        return Vec::new();
    }
    chunk_lines[1..]
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim().trim_end_matches(',');
            if trimmed.starts_with("//")
                || trimmed.is_empty()
                || trimmed == "}"
                || trimmed.starts_with('#')
            {
                return None;
            }
            let name = trimmed
                .split(|c: char| c == '(' || c == '{' || c == ' ')
                .next()?;
            if name.is_empty() { None } else { Some(name.to_owned()) }
        })
        .collect()
}

pub(crate) fn ingest_mir(
    mir_chunks: &[rude_intel::mir_edges::MirChunk],
    db_path: &Path,
    entries: &mut Vec<CodeChunkEntry>,
    file_metadata_map: &mut HashMap<String, (u64, u64, Vec<u64>)>,
    changed_sources: Option<&std::collections::HashSet<String>>,
) -> Result<(), anyhow::Error> {
    use rude_db::file_utils::{generate_id, normalize_source};
    use rude_intel::parse::normalize_path;

    // Prefer prod over test when the same name appears in the same file.
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
        let workspace = db_parent.canonicalize().unwrap_or_else(|_| db_parent.to_path_buf());
        let file_path = workspace.join(file_key);
        let source = if file_path.exists() {
            normalize_source(&file_path)
        } else {
            normalize_source(Path::new(file_key))
        };
        if let Some(changed) = changed_sources {
            if !changed.contains(&source) { continue; }
        }
        let mtime = rude_db::file_utils::get_file_mtime(&file_path).unwrap_or(0);
        let size = rude_db::file_index::get_file_size(&file_path).unwrap_or(0);
        let ext = std::path::Path::new(file_key).extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = rude_db::lang_for_ext(ext);

        // Read file lines once; needed when sqlite stores empty body (no-body mode).
        let file_lines: Option<Vec<String>> = if indices.iter().any(|&i| deduped[i].body.is_empty()) {
            std::fs::read_to_string(&file_path).ok().map(|content| {
                content.lines().map(|l| l.to_owned()).collect()
            })
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

            if parsed_chunk.text.is_empty() {
                if let Some(ref lines) = file_lines {
                    let start = mc.start_line.saturating_sub(1);
                    let end = mc.end_line.min(lines.len());
                    if start < end { parsed_chunk.text = lines[start..end].join("\n"); }
                }
            }

            if parsed_chunk.signature.is_none() {
                parsed_chunk.signature = parsed_chunk.text.lines().next()
                    .and_then(|l| { let s = l.split('{').next()?.trim(); (!s.is_empty()).then(|| s.to_owned()) });
            }

            let chunk_lines_vec: Vec<&str> = parsed_chunk.text.lines().collect();
            parsed_chunk.param_types = parse_param_types(parsed_chunk.signature.as_deref());
            parsed_chunk.return_type = parse_return_type(parsed_chunk.signature.as_deref());
            if parsed_chunk.kind == "struct" { parsed_chunk.field_types = parse_field_types(&chunk_lines_vec); }
            if parsed_chunk.kind == "enum" { parsed_chunk.enum_variants = parse_enum_variants(&chunk_lines_vec); }

            if parsed_chunk.visibility.is_empty() {
                let fl = chunk_lines_vec.first().map(|l| l.trim()).unwrap_or("");
                parsed_chunk.visibility = ["pub(crate)", "pub(super)", "pub"].iter()
                    .find(|p| fl.starts_with(**p)).map(|p| (*p).to_owned()).unwrap_or_default();
            }

            entries.push(CodeChunkEntry {
                chunk: parsed_chunk,
                source: source.clone(),
                mtime,
                lang,
            });
        }

        file_metadata_map.insert(source.clone(), (mtime, size, chunk_ids));
    }

    Ok(())
}

/// Removes stale chunks, inserts new chunks in batch, and updates `file_idx` in memory.
/// The caller is responsible for calling `file_index::save_file_index` afterwards.
pub(crate) fn write_chunks(
    entries: &[CodeChunkEntry],
    engine: &mut rude_db::StorageEngine,
    file_metadata_map: &HashMap<String, (u64, u64, Vec<u64>)>,
    file_idx: &mut FileIndex,
    include_content_hash: bool,
) -> Result<u64> {
    use rude_db::Payload;

    // Remove stale chunks for files that are being re-indexed.
    for (path, (_, _, new_ids)) in file_metadata_map {
        if let Some(existing) = file_idx.get_file(path) {
            let new_id_set: std::collections::HashSet<u64> = new_ids.iter().copied().collect();
            for &old_id in &existing.chunk_ids {
                if !new_id_set.contains(&old_id) {
                    let _ = engine.remove(old_id);
                }
            }
        }
    }

    // Build callers and per-file chunk counts.
    let reverse_index = build_callers(entries);
    let mut chunk_total_map: HashMap<&str, usize> = HashMap::new();
    for entry in entries {
        *chunk_total_map.entry(entry.source.as_str()).or_default() += 1;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    use rayon::prelude::*;
    let encoded: Vec<(u64, Payload, String)> = entries.par_iter().map(|entry| {
        let chunk_total = chunk_total_map.get(entry.source.as_str()).copied().unwrap_or(1);
        build_payload(entry, now, chunk_total, &reverse_index, false)
    }).collect();
    let batch: Vec<(u64, Payload, &str)> = encoded.iter()
        .map(|(id, payload, text)| (*id, payload.clone(), text.as_str())).collect();
    engine.insert_batch(&batch).context("Failed to bulk load")?;

    // Update in-memory file index.
    for (path, (mtime, size, chunk_ids)) in file_metadata_map {
        let hash = if include_content_hash {
            Some(rude_db::file_utils::content_hash(std::path::Path::new(path)).unwrap_or(0))
        } else {
            None
        };
        file_idx.update_file(path.to_string(), *mtime, *size, chunk_ids.clone(), hash);
    }

    Ok(entries.len() as u64)
}
