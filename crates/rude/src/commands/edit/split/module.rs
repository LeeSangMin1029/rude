use std::path::Path;
use anyhow::{Context, Result, bail};
use crate::commands::edit::locate::{SymbolLocation, locate_symbol_in};
use super::{sort_by_kind, is_header_line, filter_header};

pub fn split_module(file: String, targets: Vec<String>, dry_run: bool) -> Result<()> {
    let db = crate::db();
    let root = rude_util::find_project_root(db).context("Cannot determine project root")?;
    let graph = crate::commands::intel::load_or_build_graph()?;
    let parsed: Vec<(String, Vec<String>)> = targets.iter().map(|t| {
        let (file_part, syms) = t.split_once(':')
            .with_context(|| format!("Invalid target format '{t}', expected 'file.rs:sym1,sym2'"))?;
        let sym_list: Vec<String> = syms.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if sym_list.is_empty() { bail!("No symbols specified for {file_part}"); }
        Ok((file_part.to_string(), sym_list))
    }).collect::<Result<_>>()?;
    let src_chunk = graph.chunks.iter()
        .find(|c| c.file.ends_with(&file) || file.ends_with(&c.file))
        .with_context(|| format!("No file matching '{file}' in DB"))?;
    let abs_src = crate::commands::edit::file::resolve_abs_path(db, &src_chunk.file)?;
    if !abs_src.exists() { bail!("File not found: {}", abs_src.display()); }
    let source_content = std::fs::read_to_string(&abs_src)?;
    let source_lines: Vec<&str> = source_content.lines().collect();
    let stem = abs_src.file_stem().and_then(|s| s.to_str()).context("Cannot get file stem")?;
    let is_already_dir = stem == "mod" || stem == "lib" || stem == "main";
    let (dir, mod_rs_path) = if is_already_dir {
        (abs_src.parent().unwrap().to_path_buf(), abs_src.clone())
    } else {
        let d = abs_src.parent().unwrap().join(stem);
        (d.clone(), d.join("mod.rs"))
    };
    // resolve all symbols
    let file_hint = Some(src_chunk.file.as_str());
    let mut all_locs: Vec<(usize, SymbolLocation)> = Vec::new();
    for (ti, (_, syms)) in parsed.iter().enumerate() {
        for sym in syms {
            let loc = locate_symbol_in(&graph, db, sym, file_hint)
                .with_context(|| format!("Symbol '{sym}' not found in {file}"))?;
            all_locs.push((ti, loc));
        }
    }
    // warn if struct/trait and its impl are in different targets
    {
        let type_names: std::collections::HashMap<&str, usize> = all_locs.iter()
            .filter(|(_, loc)| matches!(loc.kind.as_str(), "struct" | "enum" | "trait"))
            .map(|(ti, loc)| {
                let leaf = loc.rel_path.rsplit("::").next().unwrap_or(&loc.rel_path);
                (leaf, *ti)
            }).collect();
        for (ti, loc) in &all_locs {
            if loc.kind == "impl" {
                for (type_name, type_ti) in &type_names {
                    if loc.rel_path.contains(type_name) && ti != type_ti {
                        eprintln!("  warning: impl for '{type_name}' (target {}) is separated from its type definition (target {})",
                            parsed[*ti].0, parsed[*type_ti].0);
                    }
                }
            }
        }
    }
    let mut all_moved_ranges: Vec<(usize, usize)> = Vec::new();
    let crate_prefix = compute_crate_prefix(&abs_src, &root);
    // collect inner attrs
    let inner_attrs: Vec<String> = source_content.lines()
        .take_while(|l| is_header_line(l.trim()))
        .filter(|l| l.trim().starts_with("#!["))
        .map(|l| l.to_string())
        .collect();
    // build each target file
    struct TargetFile { rel_path: String, content: String, symbols: Vec<String> }
    let mut target_files: Vec<TargetFile> = Vec::new();
    for (ti, (target_name, syms)) in parsed.iter().enumerate() {
        let locs: Vec<&SymbolLocation> = all_locs.iter()
            .filter(|(idx, _)| *idx == ti)
            .map(|(_, loc)| loc)
            .collect();
        let mut ranges: Vec<(usize, usize, &str, &str)> = locs.iter()
            .zip(syms.iter())
            .map(|(loc, name)| (loc.start_line, loc.end_line, name.as_str(), loc.kind.as_str()))
            .collect();
        sort_by_kind(&mut ranges);
        // determine visibility for each symbol
        let vis_map: std::collections::HashMap<&str, &str> = ranges.iter().map(|&(_, _, name, _)| {
            let orig_vis = get_original_visibility(&source_lines, ranges.iter()
                .find(|r| r.2 == name).map(|r| r.0).unwrap_or(0));
            (name, orig_vis)
        }).collect();
        let moved_body: String = ranges.iter()
            .flat_map(|&(start, end, _, _)| &source_lines[start..=end])
            .cloned().collect::<Vec<_>>().join("\n");
        let (_, use_lines) = filter_header(&source_content, &moved_body);
        let mut parts: Vec<String> = Vec::new();
        if !inner_attrs.is_empty() { parts.extend(inner_attrs.iter().cloned()); parts.push(String::new()); }
        let fixed_uses: Vec<String> = use_lines.iter()
            .map(|l| fix_paths(l, &crate_prefix, is_already_dir))
            .collect();
        if !fixed_uses.is_empty() { parts.extend(fixed_uses); }
        // add cross-module imports for symbols referenced but in other targets or mod.rs
        let mut cross_imports: Vec<String> = Vec::new();
        // check which symbols from other targets or remaining are used in this target's body
        for (oti, (other_target, other_syms)) in parsed.iter().enumerate() {
            if oti == ti { continue; }
            let other_mod = Path::new(other_target.as_str()).file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let used: Vec<&str> = other_syms.iter()
                .filter(|s| contains_word(&moved_body, s.as_str()))
                .map(|s| s.as_str()).collect();
            if !used.is_empty() {
                cross_imports.push(format!("use super::{other_mod}::{{{}}};", used.join(", ")));
            }
        }
        // check remaining symbols (in mod.rs) used by this target
        {
            let all_moved_syms: std::collections::HashSet<&str> = parsed.iter()
                .flat_map(|(_, syms)| syms.iter().map(|s| s.as_str()))
                .collect();
            let remaining_in_mod: Vec<&str> = graph.chunks.iter()
                .filter(|c| c.file == src_chunk.file)
                .filter(|c| {
                    let leaf = c.name.rsplit("::").next().unwrap_or(&c.name);
                    !all_moved_syms.contains(leaf)
                })
                .filter(|c| matches!(c.kind.as_str(), "function" | "struct" | "enum" | "trait"))
                .map(|c| c.name.rsplit("::").next().unwrap_or(&c.name))
                .filter(|name| contains_word(&moved_body, name))
                .collect();
            if !remaining_in_mod.is_empty() {
                cross_imports.push(format!("use super::{{{}}};", remaining_in_mod.join(", ")));
            }
        }
        if !cross_imports.is_empty() { parts.extend(cross_imports); }
        if !parts.is_empty() && !parts.last().is_some_and(|l| l.is_empty()) { parts.push(String::new()); }
        for (ri, &(start, end, name, _)) in ranges.iter().enumerate() {
            if ri > 0 { parts.push(String::new()); }
            for (li, idx) in (start..=end).enumerate() {
                let mut line = source_lines[idx].to_string();
                // fix super:: paths in function body
                if !is_already_dir && line.contains("super::") {
                    line = fix_paths(&line, &crate_prefix, false);
                }
                // fix visibility on first line of function
                if li == 0 {
                    let orig_vis = vis_map.get(name).copied().unwrap_or("");
                    line = ensure_visibility(&line, orig_vis);
                }
                parts.push(line);
            }
        }
        let content = parts.join("\n") + "\n";
        for &(start, end, _, _) in &ranges {
            all_moved_ranges.push((start, end));
        }
        target_files.push(TargetFile { rel_path: target_name.clone(), content, symbols: syms.clone() });
    }
    if dry_run {
        eprintln!("=== DRY RUN: split-module {} ===", file);
        if !is_already_dir { eprintln!("  rename {} → {}/mod.rs", file, stem); }
        for tf in &target_files {
            eprintln!("  create {}/{} ({} lines, {} symbols: {})",
                stem, tf.rel_path, tf.content.lines().count(), tf.symbols.len(), tf.symbols.join(", "));
        }
        eprintln!("  mod.rs: mod declarations + pub use + remaining code");
        return Ok(());
    }
    // execute: rename to dir if needed
    if !is_already_dir {
        std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
        std::fs::rename(&abs_src, &mod_rs_path)
            .with_context(|| format!("Failed to rename {} → {}", abs_src.display(), mod_rs_path.display()))?;
        let rel_src = abs_src.strip_prefix(&root).unwrap_or(&abs_src);
        eprintln!("Renamed {} → {}/mod.rs", rel_src.display(), stem);
    }
    // write target files
    for tf in &target_files {
        let target_abs = dir.join(&tf.rel_path);
        if target_abs.exists() { bail!("Target file already exists: {}", target_abs.display()); }
        std::fs::write(&target_abs, &tf.content)?;
        eprintln!("Created {}/{} ({} line(s))", stem, tf.rel_path, tf.content.lines().count());
    }
    // rebuild mod.rs in one pass (no splice + locked_edit two-step)
    let mod_content = build_mod_rs(
        &source_content, &source_lines, &all_moved_ranges,
        &target_files.iter().map(|tf| (tf.rel_path.as_str(), tf.symbols.as_slice())).collect::<Vec<_>>(),
        &crate_prefix, is_already_dir,
    );
    std::fs::write(&mod_rs_path, &mod_content)?;
    eprintln!("Updated {}/mod.rs", stem);
    // clean unused imports in all generated files
    for tf in &target_files {
        let target_abs = dir.join(&tf.rel_path);
        crate::commands::edit::imports::cleanup_unused_imports(&target_abs).ok();
    }
    crate::commands::edit::imports::cleanup_unused_imports(&mod_rs_path).ok();
    Ok(())
}

fn build_mod_rs(
    source: &str, source_lines: &[&str], moved_ranges: &[(usize, usize)],
    targets: &[(&str, &[String])],
    crate_prefix: &str, is_already_dir: bool,
) -> String {
    let moved_set: std::collections::HashSet<(usize, usize)> = moved_ranges.iter().copied().collect();
    let mut result: Vec<String> = Vec::new();
    // 1. inner attrs
    for line in source.lines() {
        let t = line.trim();
        if !is_header_line(t) { break; }
        if t.starts_with("#![") { result.push(line.to_string()); }
    }
    if !result.is_empty() { result.push(String::new()); }
    // 2. mod declarations (existing + new)
    for line in source.lines() {
        let t = line.trim();
        if !is_header_line(t) { break; }
        if (t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("pub(crate) mod ")) && t.ends_with(';') {
            result.push(line.to_string());
        }
    }
    for (target_name, _) in targets {
        let mod_name = Path::new(target_name).file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let decl = format!("mod {mod_name};");
        if !result.iter().any(|l| l.trim() == decl || l.trim() == format!("pub mod {mod_name};")) {
            result.push(decl);
        }
    }
    result.push(String::new());
    // 3. re-exports for moved pub/pub(crate) symbols, preserving original visibility
    for (target_name, syms) in targets {
        let mod_name = Path::new(target_name).file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let mut pub_syms: Vec<&str> = Vec::new();
        let mut pub_crate_syms: Vec<&str> = Vec::new();
        for s in syms.iter() {
            let name = s.as_str();
            let vis = source_lines.iter().find_map(|l| {
                let t = l.trim();
                if !t.contains(name) { return None; }
                if t.starts_with("pub(crate) fn ") || t.starts_with("pub(crate) struct ")
                    || t.starts_with("pub(crate) enum ") || t.starts_with("pub(crate) trait ") { Some("pub(crate)") }
                else if t.starts_with("pub fn ") || t.starts_with("pub struct ")
                    || t.starts_with("pub enum ") || t.starts_with("pub trait ") { Some("pub") }
                else { None }
            });
            match vis {
                Some("pub(crate)") => pub_crate_syms.push(name),
                Some("pub") => pub_syms.push(name),
                _ => {}
            }
        }
        if pub_syms.len() == 1 { result.push(format!("pub use {mod_name}::{};", pub_syms[0])); }
        else if !pub_syms.is_empty() { result.push(format!("pub use {mod_name}::{{{}}};", pub_syms.join(", "))); }
        if pub_crate_syms.len() == 1 { result.push(format!("pub(crate) use {mod_name}::{};", pub_crate_syms[0])); }
        else if !pub_crate_syms.is_empty() { result.push(format!("pub(crate) use {mod_name}::{{{}}};", pub_crate_syms.join(", "))); }
    }
    // 4. remaining code (not moved) with its use statements
    let mut remaining_lines: Vec<String> = Vec::new();
    let mut in_header = true;
    for (i, &line) in source_lines.iter().enumerate() {
        let t = line.trim();
        if in_header {
            if is_header_line(t) { continue; }
            in_header = false;
        }
        let in_moved = moved_set.iter().any(|&(ms, me)| i >= ms && i <= me);
        if !in_moved { remaining_lines.push(line.to_string()); }
    }
    while remaining_lines.last().is_some_and(|l| l.trim().is_empty()) { remaining_lines.pop(); }
    while remaining_lines.first().is_some_and(|l| l.trim().is_empty()) { remaining_lines.remove(0); }
    if !remaining_lines.is_empty() {
        let body = remaining_lines.join("\n");
        let remaining_uses = filter_header(source, &body).1;
        if !remaining_uses.is_empty() {
            result.push(String::new());
            for u in &remaining_uses {
                result.push(fix_paths(u, crate_prefix, is_already_dir));
            }
        }
        // import moved symbols that remaining code references
        for (target_name, syms) in targets {
            let mod_name = Path::new(target_name).file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let used: Vec<&str> = syms.iter()
                .filter(|s| contains_word(&body, s.as_str()))
                .map(|s| s.as_str()).collect();
            if !used.is_empty() {
                result.push(format!("use {mod_name}::{{{}}};", used.join(", ")));
            }
        }
        result.push(String::new());
        for line in &remaining_lines {
            result.push(fix_paths(line, crate_prefix, is_already_dir));
        }
    }
    result.push(String::new());
    result.join("\n")
}

fn compute_crate_prefix(abs_src: &Path, _root: &Path) -> String {
    let path_str = abs_src.to_string_lossy().replace('\\', "/");
    let after_src = path_str.rsplit_once("/src/")
        .map(|(_, after)| after)
        .unwrap_or("");
    let parts: Vec<&str> = after_src.split('/')
        .filter(|c| !c.ends_with(".rs") && !c.is_empty())
        .collect();
    if parts.is_empty() { "crate::".to_string() }
    else { format!("crate::{}::", parts.join("::")) }
}

fn fix_paths(line: &str, crate_prefix: &str, is_already_dir: bool) -> String {
    if is_already_dir { return line.to_string(); }
    let trimmed = line.trim();
    // skip comments and string literals containing paths
    if trimmed.starts_with("//") || trimmed.starts_with("///") { return line.to_string(); }
    // for lines with string literals, only replace outside quotes
    if line.contains('"') {
        return replace_outside_strings(line, &[("super::", crate_prefix), ("self::", "super::")]);
    }
    let mut result = line.to_string();
    if result.contains("super::") { result = result.replace("super::", crate_prefix); }
    if result.contains("self::") { result = result.replace("self::", "super::"); }
    result
}

fn replace_outside_strings(line: &str, replacements: &[(&str, &str)]) -> String {
    let mut result = String::with_capacity(line.len());
    let mut in_string = false;
    let mut chars = line.chars().peekable();
    let mut buf = String::new();
    while let Some(ch) = chars.next() {
        if ch == '"' && !buf.ends_with('\\') {
            if in_string {
                result.push('"');
                in_string = false;
            } else {
                // flush buf with replacements
                let mut replaced = buf.clone();
                for &(from, to) in replacements {
                    replaced = replaced.replace(from, to);
                }
                result.push_str(&replaced);
                buf.clear();
                result.push('"');
                in_string = true;
            }
        } else if in_string {
            result.push(ch);
        } else {
            buf.push(ch);
        }
    }
    if !buf.is_empty() {
        let mut replaced = buf;
        for &(from, to) in replacements {
            replaced = replaced.replace(from, to);
        }
        result.push_str(&replaced);
    }
    result
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    for (i, _) in haystack.match_indices(needle) {
        let after = i + needle.len();
        let before_ok = i == 0 || !haystack.as_bytes()[i - 1].is_ascii_alphanumeric() && haystack.as_bytes()[i - 1] != b'_';
        let after_ok = after >= haystack.len() || !haystack.as_bytes()[after].is_ascii_alphanumeric() && haystack.as_bytes()[after] != b'_';
        if before_ok && after_ok { return true; }
    }
    false
}

fn get_original_visibility(source_lines: &[&str], start: usize) -> &'static str {
    let line = source_lines.get(start).map(|l| l.trim()).unwrap_or("");
    if line.starts_with("pub(crate) ") { "pub(crate)" }
    else if line.starts_with("pub ") { "pub" }
    else { "" }
}

pub fn split_module_auto(file: String, dry_run: bool) -> Result<()> {
    let db = crate::db();
    let graph = crate::commands::intel::load_or_build_graph()?;
    let src_chunk = graph.chunks.iter()
        .find(|c| c.file.ends_with(&file) || file.ends_with(&c.file))
        .with_context(|| format!("No file matching '{file}' in DB"))?;
    let src_file = src_chunk.file.clone();
    // collect all functions in this file
    let file_chunks: Vec<(u32, &str, &str, bool)> = graph.chunks.iter().enumerate()
        .filter(|(_, c)| c.file == src_file)
        .filter(|(_, c)| matches!(c.kind.as_str(), "function" | "struct" | "enum" | "trait" | "impl"))
        .map(|(i, c)| {
            let leaf = c.name.rsplit("::").next().unwrap_or(&c.name);
            let is_pub = c.kind == "function" && {
                let abs = crate::commands::edit::file::resolve_abs_path(db, &c.file).ok();
                abs.and_then(|p| std::fs::read_to_string(&p).ok()).map_or(false, |content| {
                    if let Some((s, _)) = c.lines {
                        content.lines().nth(s.saturating_sub(1)).map_or(false, |l| {
                            let t = l.trim();
                            t.starts_with("pub fn ") || t.starts_with("pub(crate) fn ")
                        })
                    } else { false }
                })
            };
            (i as u32, leaf, c.kind.as_str(), is_pub)
        })
        .collect();
    // find entry points: pub/pub(crate) functions called from outside this file
    let entry_points: Vec<u32> = file_chunks.iter()
        .filter(|(_, leaf, kind, is_pub)| {
            *kind == "function" && *is_pub && !leaf.contains("::")
        })
        .map(|(idx, _, _, _)| *idx)
        .collect();
    if entry_points.is_empty() {
        eprintln!("No pub entry points found in {file}. Nothing to split.");
        return Ok(());
    }
    // build groups: for each entry point, find private fns it exclusively calls
    let file_idx_set: std::collections::HashSet<u32> = file_chunks.iter().map(|(i, _, _, _)| *i).collect();
    let mut assigned: std::collections::HashMap<u32, usize> = std::collections::HashMap::new(); // chunk_idx → group_idx
    // first pass: assign entry points to their own groups
    let mut groups: Vec<(String, Vec<String>)> = Vec::new(); // (filename, symbols)
    for &ep in &entry_points {
        let name = graph.chunks[ep as usize].name.rsplit("::").next().unwrap_or(&graph.chunks[ep as usize].name);
        let gi = groups.len();
        groups.push((format!("{name}.rs"), vec![name.to_string()]));
        assigned.insert(ep, gi);
    }
    // second pass: for each non-pub function, find which entry points call it
    let private_fns: Vec<u32> = file_chunks.iter()
        .filter(|(_, _, kind, is_pub)| *kind == "function" && !*is_pub)
        .map(|(idx, _, _, _)| *idx)
        .collect();
    for &pf in &private_fns {
        let callers_in_file: Vec<u32> = graph.callers[pf as usize].iter()
            .filter(|&&c| file_idx_set.contains(&c))
            .copied()
            .collect();
        // find which groups call this function
        let caller_groups: std::collections::HashSet<usize> = callers_in_file.iter()
            .filter_map(|c| assigned.get(c))
            .copied()
            .collect();
        if caller_groups.len() == 1 {
            let gi = *caller_groups.iter().next().unwrap();
            let name = graph.chunks[pf as usize].name.rsplit("::").next().unwrap_or(&graph.chunks[pf as usize].name);
            groups[gi].1.push(name.to_string());
            assigned.insert(pf, gi);
        }
        // if called by 0 or 2+ groups → stays in mod.rs (not assigned)
    }
    // third pass: assign struct/enum/trait to groups if used exclusively by one group
    let types: Vec<u32> = file_chunks.iter()
        .filter(|(_, _, kind, _)| matches!(*kind, "struct" | "enum" | "trait"))
        .map(|(idx, _, _, _)| *idx)
        .collect();
    for &ti in &types {
        let type_name = graph.chunks[ti as usize].name.rsplit("::").next().unwrap_or(&graph.chunks[ti as usize].name);
        // check which groups reference this type (by name in source)
        let abs = crate::commands::edit::file::resolve_abs_path(db, &src_file).ok();
        let source = abs.and_then(|p| std::fs::read_to_string(&p).ok()).unwrap_or_default();
        let mut using_groups: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for (gi, (_, syms)) in groups.iter().enumerate() {
            for sym in syms {
                if let Some(loc) = graph.chunks.iter().enumerate()
                    .find(|(_, c)| c.file == src_file && c.name.ends_with(sym)) {
                    if let Some((s, e)) = loc.1.lines {
                        let body: String = source.lines().skip(s.saturating_sub(1)).take(e - s.saturating_sub(1) + 1).collect::<Vec<_>>().join("\n");
                        if contains_word(&body, type_name) { using_groups.insert(gi); }
                    }
                }
            }
        }
        if using_groups.len() == 1 {
            let gi = *using_groups.iter().next().unwrap();
            groups[gi].1.push(type_name.to_string());
            assigned.insert(ti, gi);
        }
    }
    // compute total lines per group, dissolve small groups back to mod.rs
    let min_group_lines = crate::config::get().cluster.min_lines;
    let abs = crate::commands::edit::file::resolve_abs_path(db, &src_file).ok();
    let source_for_lines = abs.and_then(|p| std::fs::read_to_string(&p).ok()).unwrap_or_default();
    for gi in 0..groups.len() {
        let total: usize = groups[gi].1.iter().filter_map(|sym| {
            graph.chunks.iter().find(|c| c.file == src_file && c.name.ends_with(sym))
                .and_then(|c| c.lines.map(|(s, e)| e - s + 1))
        }).sum();
        if total < min_group_lines {
            for sym_idx in file_chunks.iter().filter(|(_, name, _, _)| groups[gi].1.contains(&name.to_string())).map(|(i, _, _, _)| *i) {
                assigned.remove(&sym_idx);
            }
            groups[gi].1.clear();
        }
    }
    groups.retain(|(_, syms)| !syms.is_empty());
    let targets: Vec<String> = groups.iter()
        .map(|(fname, syms)| format!("{fname}:{}", syms.join(",")))
        .collect();
    if targets.is_empty() {
        eprintln!("No groups formed. Nothing to split.");
        return Ok(());
    }
    eprintln!("=== auto-plan for {file} ===");
    for (fname, syms) in &groups {
        if !syms.is_empty() { eprintln!("  {fname}: {}", syms.join(", ")); }
    }
    let remaining: Vec<&str> = file_chunks.iter()
        .filter(|(idx, _, _, _)| !assigned.contains_key(idx))
        .map(|(_, name, _, _)| *name)
        .collect();
    if !remaining.is_empty() { eprintln!("  mod.rs: {}", remaining.join(", ")); }
    split_module(file, targets, dry_run)
}

fn ensure_visibility(line: &str, orig_vis: &str) -> String {
    let trimmed = line.trim_start();
    if trimmed.starts_with("pub ") || trimmed.starts_with("pub(") { return line.to_string(); }
    if orig_vis.is_empty() {
        // was private — make pub(super) so sibling sub-modules can call it
        if trimmed.starts_with("fn ") {
            let indent = &line[..line.len() - trimmed.len()];
            return format!("{indent}pub(super) {trimmed}");
        }
        return line.to_string();
    }
    line.to_string()
}
