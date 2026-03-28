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
    // collect moved line ranges
    let mut all_moved_ranges: Vec<(usize, usize)> = Vec::new();
    // compute super:: replacement: if file becomes dir module, depth increases by 1
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
            .map(|l| fix_super_path(l, &crate_prefix, is_already_dir))
            .collect();
        if !fixed_uses.is_empty() { parts.extend(fixed_uses); }
        // add cross-module imports for symbols referenced but in other targets or mod.rs
        let mut cross_imports: Vec<String> = Vec::new();
        // check which symbols from other targets or remaining are used in this target's body
        for (oti, (other_target, other_syms)) in parsed.iter().enumerate() {
            if oti == ti { continue; }
            let other_mod = Path::new(other_target.as_str()).file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let used: Vec<&str> = other_syms.iter()
                .filter(|s| moved_body.contains(s.as_str()))
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
                .filter(|c| c.kind == "function")
                .map(|c| c.name.rsplit("::").next().unwrap_or(&c.name))
                .filter(|name| moved_body.contains(name))
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
                    line = fix_super_path(&line, &crate_prefix, false);
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
        eprintln!("=== DRY RUN: split-module {} ===\n", file);
        if !is_already_dir { eprintln!("Step 1: Rename {} → {}/mod.rs\n", file, stem); }
        for tf in &target_files {
            eprintln!("--- {} ({} line(s), {} symbols) ---", tf.rel_path, tf.content.lines().count(), tf.symbols.len());
            for (i, line) in tf.content.lines().enumerate() { eprintln!("{:>4}| {line}", i + 1); }
            eprintln!();
        }
        eprintln!("--- mod.rs ---");
        eprintln!("  (mod declarations + pub use + remaining code)");
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
    // 3. pub use re-exports for moved pub symbols
    for (target_name, syms) in targets {
        let mod_name = Path::new(target_name).file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let pub_syms: Vec<&str> = syms.iter()
            .filter(|s| {
                let name = s.as_str();
                source_lines.iter().any(|l| {
                    let t = l.trim();
                    (t.starts_with("pub fn ") || t.starts_with("pub(crate) fn ") || t.starts_with("pub struct ")
                        || t.starts_with("pub enum ") || t.starts_with("pub trait "))
                        && t.contains(name)
                })
            })
            .map(|s| s.as_str()).collect();
        if pub_syms.len() == 1 {
            result.push(format!("pub use {mod_name}::{};", pub_syms[0]));
        } else if !pub_syms.is_empty() {
            result.push(format!("pub use {mod_name}::{{{}}};", pub_syms.join(", ")));
        }
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
                result.push(fix_super_path(u, crate_prefix, is_already_dir));
            }
        }
        // import moved symbols that remaining code references
        for (target_name, syms) in targets {
            let mod_name = Path::new(target_name).file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let used: Vec<&str> = syms.iter()
                .filter(|s| body.contains(s.as_str()))
                .map(|s| s.as_str()).collect();
            if !used.is_empty() {
                result.push(format!("use {mod_name}::{{{}}};", used.join(", ")));
            }
        }
        result.push(String::new());
        for line in &remaining_lines {
            result.push(fix_super_path(line, crate_prefix, is_already_dir));
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

fn fix_super_path(line: &str, crate_prefix: &str, is_already_dir: bool) -> String {
    if is_already_dir || !line.contains("super::") { return line.to_string(); }
    line.replace("super::", crate_prefix)
}

fn get_original_visibility(source_lines: &[&str], start: usize) -> &'static str {
    let line = source_lines.get(start).map(|l| l.trim()).unwrap_or("");
    if line.starts_with("pub(crate) ") { "pub(crate)" }
    else if line.starts_with("pub ") { "pub" }
    else { "" }
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
