use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rude_intel::chunk_types as chunk_code;

pub(crate) struct CodeChunkEntry {
    pub chunk: chunk_code::CodeChunk,
    pub source: String,
    pub file_path_str: String,
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
    tags.push(format!("kind:{}", chunk.kind.as_str()));
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

    let embed_text = chunk.to_embed_text(&entry.file_path_str, &called_by_strings);

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

            let body_text = if mc.body.is_empty() {
                if let Some(ref lines) = file_lines {
                    let start = mc.start_line.saturating_sub(1);
                    let end = mc.end_line.min(lines.len());
                    if start < end {
                        lines[start..end].join("\n")
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                mc.body.clone()
            };

            let (calls, call_lines) = rude_intel::mir_edges::parse_calls_field(&mc.calls);

            let kind = match mc.kind.as_str() {
                "fn" | "method" => chunk_code::CodeNodeKind::Function,
                "struct" => chunk_code::CodeNodeKind::Struct,
                "enum" => chunk_code::CodeNodeKind::Enum,
                "trait" => chunk_code::CodeNodeKind::Trait,
                "impl" => chunk_code::CodeNodeKind::Impl,
                _ => chunk_code::CodeNodeKind::Function,
            };

            let type_refs: Vec<String> = if mc.type_refs.is_empty() {
                Vec::new()
            } else {
                mc.type_refs.split(", ").map(|s| s.to_owned()).collect()
            };

            let chunk_lines: Vec<&str> = body_text.lines().collect();

            // Fallback to first line of body when MIR signature is absent.
            let signature = mc.signature.clone().or_else(|| {
                let first = chunk_lines.first()?;
                let sig_line = first.split('{').next()?.trim();
                if sig_line.is_empty() { None } else { Some(sig_line.to_owned()) }
            });

            let param_types = parse_param_types(signature.as_deref());
            let return_type = parse_return_type(signature.as_deref());
            let field_types = if kind == chunk_code::CodeNodeKind::Struct {
                parse_field_types(&chunk_lines)
            } else {
                Vec::new()
            };
            let enum_variants = if kind == chunk_code::CodeNodeKind::Enum {
                parse_enum_variants(&chunk_lines)
            } else {
                Vec::new()
            };

            let visibility = mc
                .visibility
                .clone()
                .unwrap_or_else(|| {
                    let first_line = chunk_lines.first().map(|l| l.trim()).unwrap_or("");
                    if first_line.starts_with("pub(crate)") {
                        "pub(crate)".to_owned()
                    } else if first_line.starts_with("pub(super)") {
                        "pub(super)".to_owned()
                    } else if first_line.starts_with("pub") {
                        "pub".to_owned()
                    } else {
                        String::new()
                    }
                });

            let is_test = mc.is_test
                || mc.file.contains("/tests/") || mc.file.contains("\\tests\\")
                || mc.name.contains("::test_") || mc.name.starts_with("test_")
                || chunk_lines
                    .first()
                    .is_some_and(|l| l.contains("#[test]") || l.contains("#[cfg(test)]"));

            let code_chunk = chunk_code::CodeChunk {
                text: body_text.clone(),
                kind,
                name: mc.name.clone(),
                signature,
                calls,
                call_lines,
                type_refs,
                start_line: mc.start_line.saturating_sub(1),
                end_line: mc.end_line.saturating_sub(1),
                start_byte: 0,
                end_byte: body_text.len(),
                chunk_index: idx,
                imports: Vec::new(),
                visibility,
                string_args: Vec::new(),
                param_flows: Vec::new(),
                param_types,
                field_types,
                local_types: Vec::new(),
                let_call_bindings: Vec::new(),
                return_type,
                field_accesses: Vec::new(),
                enum_variants,
                is_test,
                sub_blocks: Vec::new(),
                ast_hash: 0,
                body_hash: 0,
                doc_comment: None,
            };

            entries.push(CodeChunkEntry {
                chunk: code_chunk,
                source: source.clone(),
                file_path_str: normalized_file.clone(),
                mtime,
                lang,
            });
        }

        file_metadata_map.insert(source.clone(), (mtime, size, chunk_ids));
    }

    Ok(())
}
