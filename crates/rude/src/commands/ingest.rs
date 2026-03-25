//! Code-specific ingestion utilities.
//!
//! Chunks code files via MIR, converts to CodeChunkEntry for DB storage.

use std::collections::HashMap;
use std::path::Path;

use rude_intel::chunk_types as chunk_code;
use rude_db::file_index;

/// Intermediate data collected per code chunk before `called_by` resolution.
pub struct CodeChunkEntry {
    pub chunk: chunk_code::CodeChunk,
    pub source: String,
    pub file_path_str: String,
    pub mtime: u64,
    pub lang: &'static str,
}

/// Build `called_by` reverse index from all chunks' `calls` data.
///
/// For each call target, extracts the bare function name (last segment after
/// `::` or `.`) and maps it to the set of callers (qualified chunk names).
pub fn build_called_by_index(entries: &[CodeChunkEntry]) -> HashMap<String, Vec<String>> {
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


/// Look up `called_by` entries for a given chunk name.
///
/// Checks both the full qualified name and the bare (last segment) name
/// against the reverse index built by [`build_called_by_index`].
pub fn lookup_called_by<'a>(
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


/// Direct MirChunk → CodeChunkEntry conversion. No file I/O.
/// Uses body/calls/type_refs from MIR extraction (sqlite or JSONL with body).
pub fn chunks_from_mir_direct(
    mir_chunks: &[rude_intel::mir_edges::MirChunk],
    db_path: &Path,
    entries: &mut Vec<CodeChunkEntry>,
    file_metadata_map: &mut HashMap<String, (u64, u64, Vec<u64>)>,
    changed_sources: Option<&std::collections::HashSet<String>>,
) -> Result<(), anyhow::Error> {
    use rude_db::file_utils::{generate_id, normalize_source};
    use rude_intel::parse::normalize_path;

    // Dedup: same name in same file, prefer prod over test
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

    // Group by file for file_metadata_map
    let mut by_file: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, mc) in deduped.iter().enumerate() {
        by_file.entry(mc.file.as_str()).or_default().push(i);
    }

    for (file_key, indices) in &by_file {
        let normalized_file = normalize_path(file_key);
        // Try to resolve absolute path for source
        let db_parent = db_path.parent().unwrap_or(Path::new("."));
        let workspace = db_parent.canonicalize().unwrap_or_else(|_| db_parent.to_path_buf());
        let file_path = workspace.join(file_key);
        let source = if file_path.exists() {
            normalize_source(&file_path)
        } else {
            normalize_source(Path::new(file_key))
        };
        // Skip unchanged files
        if let Some(changed) = changed_sources {
            if !changed.contains(&source) { continue; }
        }
        let mtime = rude_db::file_utils::get_file_mtime(&file_path).unwrap_or(0);
        let size = file_index::get_file_size(&file_path).unwrap_or(0);
        let ext = std::path::Path::new(file_key).extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = rude_db::lang_for_ext(ext);

        // Read file lines once for body recovery when body is empty (sqlite without body text)
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

            // Recover body from source file when sqlite stores empty body
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

            // Parse calls from "callee@line, callee@line, ..." format
            let (calls, call_lines): (Vec<String>, Vec<u32>) = if mc.calls.is_empty() {
                (Vec::new(), Vec::new())
            } else {
                mc.calls.split(", ").map(|token| {
                    if let Some(at) = token.rfind('@') {
                        let name = token[..at].to_owned();
                        let line = token[at+1..].parse().unwrap_or(0);
                        (name, line)
                    } else {
                        (token.to_owned(), 0u32)
                    }
                }).unzip()
            };

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

            // Use MIR signature directly; fallback to first line of body
            let signature = mc.signature.clone().or_else(|| {
                let first = chunk_lines.first()?;
                let sig_line = first.split('{').next()?.trim();
                if sig_line.is_empty() {
                    None
                } else {
                    Some(sig_line.to_owned())
                }
            });

            // Parse param_types from signature: `name: Type` patterns
            let param_types: Vec<(String, String)> = signature
                .as_deref()
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
                .unwrap_or_default();

            // Parse return_type from signature: after `->`
            let return_type: Option<String> = signature.as_deref().and_then(|sig| {
                let after_arrow = sig.split("->").nth(1)?;
                let rt = after_arrow.trim().trim_end_matches('{').trim();
                if rt.is_empty() {
                    None
                } else {
                    Some(rt.to_owned())
                }
            });

            // Parse field_types for structs: `name: Type,` patterns in body
            let field_types: Vec<(String, String)> =
                if kind == chunk_code::CodeNodeKind::Struct && chunk_lines.len() > 1 {
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
                } else {
                    Vec::new()
                };

            // Parse enum_variants
            let enum_variants: Vec<String> =
                if kind == chunk_code::CodeNodeKind::Enum && chunk_lines.len() > 1 {
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
                            if name.is_empty() {
                                None
                            } else {
                                Some(name.to_owned())
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };

            // Determine visibility
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

            // Compute is_test
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

/// Create code chunks from MIR chunk definitions + source files (legacy).
/// Reads source text from files using MIR-provided line ranges.
pub fn chunk_from_mir(
    mir_chunks: &[rude_intel::mir_edges::MirChunk],
    db_path: &Path,
    entries: &mut Vec<CodeChunkEntry>,
    file_metadata_map: &mut HashMap<String, (u64, u64, Vec<u64>)>,
    changed_sources: Option<&std::collections::HashSet<String>>,
) -> Result<(), anyhow::Error> {
    use rude_db::file_utils::{generate_id, get_file_mtime, normalize_source};

    let db_parent = db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let workspace_root = db_parent
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| db_parent.to_path_buf()));
    let mut root_str =
        rude_db::strip_unc_prefix(&workspace_root.to_string_lossy()).replace('\\', "/");
    if !root_str.ends_with('/') {
        root_str.push('/');
    }

    // Group MIR chunks by file — skip external crate files
    let mut by_file: HashMap<String, Vec<&rude_intel::mir_edges::MirChunk>> = HashMap::new();
    for mc in mir_chunks {
        // Skip external crate files (.cargo/registry, rustup toolchain, etc.)
        if mc.file.contains(".cargo") || mc.file.contains("registry") || mc.file.contains("rustup") {
            continue;
        }
        by_file.entry(mc.file.clone()).or_default().push(mc);
    }

    for (file_key, chunks) in &by_file {
        // Deduplicate: same name in same file — prefer prod (is_test=false) over test
        let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut deduped: Vec<&rude_intel::mir_edges::MirChunk> = Vec::new();
        for mc in chunks {
            if let Some(&existing_idx) = seen.get(mc.name.as_str()) {
                // If existing is test and new is prod, replace
                if deduped[existing_idx].is_test && !mc.is_test {
                    deduped[existing_idx] = mc;
                }
                // Otherwise keep existing (first wins)
            } else {
                seen.insert(&mc.name, deduped.len());
                deduped.push(mc);
            }
        }
        let chunks = &deduped;
        // Resolve file path: try relative to workspace root
        let file_path = {
            let candidate = workspace_root.join(file_key);
            if candidate.exists() {
                candidate
            } else {
                std::path::PathBuf::from(file_key)
            }
        };
        // No file I/O — body text comes directly from MIR extraction.

        let source = normalize_source(&file_path);

        // Skip unchanged files if filter is provided.
        // Match strategies (in order):
        //   1. normalize_source (canonicalized absolute path)
        //   2. root_str + file_key (non-canonicalized absolute from relative)
        //   3. UNC-stripped file_path (handles canonicalize discrepancies)
        //   4. suffix match — any changed_source ends with /file_key
        //      (catches Python/JS extractors whose MIR chunks use relative paths)
        if let Some(changed) = changed_sources {
            if !changed.contains(&source) {
                let abs_from_rel = format!("{}{}", root_str, file_key.replace('\\', "/"));
                if !changed.contains(&abs_from_rel) {
                    // Strategy 3: UNC-stripped, slash-normalized file_path without canonicalize
                    let raw_abs = rude_db::strip_unc_prefix(
                        &file_path.to_string_lossy(),
                    )
                    .replace('\\', "/");
                    if !changed.contains(&raw_abs) {
                        // Strategy 4: suffix match for relative file_key
                        let rel_key = file_key.replace('\\', "/");
                        let suffix = format!("/{rel_key}");
                        let matched = changed.iter().any(|s| s.ends_with(&suffix));
                        if !matched {
                            continue;
                        }
                    }
                }
            }
        }

        let file_path_str = file_path.to_string_lossy().to_string();
        let mtime = get_file_mtime(&file_path).unwrap_or(0);
        let size = file_index::get_file_size(&file_path).unwrap_or(0);
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = rude_db::lang_for_ext(ext);

        // Read file lines once for body recovery when body is empty (sqlite without body text)
        let file_lines: Option<Vec<String>> = if chunks.iter().any(|c| c.body.is_empty()) {
            std::fs::read_to_string(&file_path).ok().map(|content| {
                content.lines().map(|l| l.to_owned()).collect()
            })
        } else {
            None
        };

        // Imports extracted from first chunk's body (use statements at file top).
        let first_body_text = if let Some(first_mc) = chunks.first() {
            if first_mc.body.is_empty() {
                if let Some(ref lines) = file_lines {
                    let start = first_mc.start_line.saturating_sub(1);
                    let end = first_mc.end_line.min(lines.len());
                    if start < end { lines[start..end].join("\n") } else { String::new() }
                } else {
                    String::new()
                }
            } else {
                first_mc.body.clone()
            }
        } else {
            String::new()
        };
        let first_body = first_body_text.as_str();
        let imports: Vec<String> = first_body.lines()
            .take_while(|line| {
                let trimmed = line.trim();
                trimmed.is_empty()
                    || trimmed.starts_with("//")
                    || trimmed.starts_with("use ")
                    || trimmed.starts_with("pub use ")
                    || trimmed.starts_with('#')
                    || trimmed.starts_with("mod ")
                    || trimmed.starts_with("pub mod ")
                    || trimmed.starts_with("extern ")
            })
            .filter(|line| {
                let trimmed = line.trim();
                trimmed.starts_with("use ") || trimmed.starts_with("pub use ")
            })
            .map(|line| line.trim().to_owned())
            .collect();

        let mut chunk_ids = Vec::with_capacity(chunks.len());

        for (idx, mc) in chunks.iter().enumerate() {
            // Recover body from source file when sqlite stores empty body
            let body_text = if mc.body.is_empty() {
                if let Some(ref lines) = file_lines {
                    let start = mc.start_line.saturating_sub(1);
                    let end = mc.end_line.min(lines.len());
                    if start < end { lines[start..end].join("\n") } else { String::new() }
                } else {
                    String::new()
                }
            } else {
                mc.body.clone()
            };
            let text = body_text;
            let chunk_lines: Vec<&str> = text.lines().collect();

            // Parse kind
            let kind = match mc.kind.as_str() {
                "fn" | "method" => chunk_code::CodeNodeKind::Function,
                "struct" => chunk_code::CodeNodeKind::Struct,
                "enum" => chunk_code::CodeNodeKind::Enum,
                "trait" => chunk_code::CodeNodeKind::Trait,
                "impl" => chunk_code::CodeNodeKind::Impl,
                _ => chunk_code::CodeNodeKind::Function,
            };

            // Use MIR signature directly; fallback to first line of body
            let signature = mc.signature.clone().or_else(|| {
                let first = chunk_lines.first()?;
                let sig_line = first.split('{').next()?.trim();
                if sig_line.is_empty() {
                    None
                } else {
                    Some(sig_line.to_owned())
                }
            });

            // Parse param_types from signature: `name: Type` patterns
            let param_types: Vec<(String, String)> = signature
                .as_deref()
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
                .unwrap_or_default();

            // Parse return_type from signature: after `->`
            let return_type: Option<String> = signature.as_deref().and_then(|sig| {
                let after_arrow = sig.split("->").nth(1)?;
                let rt = after_arrow.trim().trim_end_matches('{').trim();
                if rt.is_empty() {
                    None
                } else {
                    Some(rt.to_owned())
                }
            });

            // Parse field_types for structs: `name: Type,` patterns in body
            let field_types: Vec<(String, String)> =
                if kind == chunk_code::CodeNodeKind::Struct && chunk_lines.len() > 1 {
                    chunk_lines[1..]
                        .iter()
                        .filter_map(|line| {
                            let trimmed = line.trim().trim_end_matches(',');
                            if trimmed.starts_with("//") || trimmed.is_empty() || trimmed == "}" {
                                return None;
                            }
                            // Strip visibility prefix
                            let stripped = trimmed
                                .strip_prefix("pub(crate) ")
                                .or_else(|| trimmed.strip_prefix("pub(super) "))
                                .or_else(|| trimmed.strip_prefix("pub "))
                                .unwrap_or(trimmed);
                            let (name, ty) = stripped.split_once(':')?;
                            Some((name.trim().to_owned(), ty.trim().to_owned()))
                        })
                        .collect()
                } else {
                    Vec::new()
                };

            // Parse enum_variants
            let enum_variants: Vec<String> =
                if kind == chunk_code::CodeNodeKind::Enum && chunk_lines.len() > 1 {
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
                            if name.is_empty() {
                                None
                            } else {
                                Some(name.to_owned())
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };

            // Determine visibility
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

            let start_byte: usize = 0;
            let end_byte: usize = text.len();

            // Compute is_test before dropping chunk_lines borrow
            let is_test = mc.is_test
                || mc.file.contains("/tests/") || mc.file.contains("\\tests\\")
                || mc.name.contains("::test_") || mc.name.starts_with("test_")
                || chunk_lines
                    .first()
                    .is_some_and(|l| l.contains("#[test]") || l.contains("#[cfg(test)]"));
            drop(chunk_lines);

            let id = generate_id(&source, idx);
            chunk_ids.push(id);

            // Parse calls from MIR "callee@line, callee@line, ..." format
            let (calls, call_lines): (Vec<String>, Vec<u32>) = if mc.calls.is_empty() {
                (Vec::new(), Vec::new())
            } else {
                mc.calls.split(", ").map(|token| {
                    if let Some(at) = token.rfind('@') {
                        let name = token[..at].to_owned();
                        let line = token[at+1..].parse().unwrap_or(0);
                        (name, line)
                    } else {
                        (token.to_owned(), 0u32)
                    }
                }).unzip()
            };

            // Parse type_refs from MIR "Type, Type, ..." format
            let type_refs: Vec<String> = if mc.type_refs.is_empty() {
                Vec::new()
            } else {
                mc.type_refs.split(", ").map(|s| s.to_owned()).collect()
            };

            let code_chunk = chunk_code::CodeChunk {
                text,
                kind,
                name: mc.name.clone(),
                signature,
                doc_comment: None,
                visibility,
                start_line: mc.start_line.saturating_sub(1),
                end_line: mc.end_line.saturating_sub(1),
                start_byte,
                end_byte,
                chunk_index: idx,
                imports: imports.clone(),
                calls,
                call_lines,
                type_refs,
                param_types,
                field_types,
                return_type,
                ast_hash: 0,
                body_hash: 0,
                sub_blocks: Vec::new(),
                string_args: Vec::new(),
                param_flows: Vec::new(),
                local_types: Vec::new(),
                let_call_bindings: Vec::new(),
                field_accesses: Vec::new(),
                enum_variants,
                is_test,
            };

            entries.push(CodeChunkEntry {
                chunk: code_chunk,
                source: source.clone(),
                file_path_str: file_path_str.clone(),
                mtime,
                lang,
            });
        }

        file_metadata_map.insert(source, (mtime, size, chunk_ids));
    }

    Ok(())
}

#[cfg(test)]
#[path = "tests/ingest.rs"]
mod tests;
