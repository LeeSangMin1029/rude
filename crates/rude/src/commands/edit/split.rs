use std::path::{Path, PathBuf};
use anyhow::{Context, Result, bail};
use super::ops::Op;
use super::locate::{SymbolLocation, locate_symbol, locate_symbol_in};
use super::file::locked_edit;

pub fn split(symbols: String, to: String, dry_run: bool) -> Result<()> {
    let db = crate::db();
    let symbol_names: Vec<&str> = symbols.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if symbol_names.is_empty() { bail!("--symbols must contain at least one symbol name"); }
    let locs: Vec<SymbolLocation> = symbol_names.iter()
        .map(|&s| locate_symbol(db, s, None))
        .collect::<Result<_>>()?;
    let source_path = &locs[0].abs_path;
    for loc in &locs[1..] {
        if loc.abs_path != *source_path {
            bail!("All symbols must be in the same file. Found '{}' and '{}'", locs[0].rel_path, loc.rel_path);
        }
    }
    let source_content = std::fs::read_to_string(source_path)
        .with_context(|| format!("Failed to read {}", source_path.display()))?;
    let source_lines: Vec<&str> = source_content.lines().collect();
    let mut ranges: Vec<(usize, usize, &str, &str)> = locs.iter()
        .zip(symbol_names.iter())
        .map(|(loc, &name)| (loc.start_line, loc.end_line, name, loc.kind.as_str()))
        .collect();
    {
        let mut by_line = ranges.clone();
        by_line.sort_by_key(|r| r.0);
        for w in by_line.windows(2) {
            if w[0].1 >= w[1].0 {
                bail!("Overlapping symbols: '{}' (L{}-{}) and '{}' (L{}-{})",
                    w[0].2, w[0].0 + 1, w[0].1 + 1, w[1].2, w[1].0 + 1, w[1].1 + 1);
            }
        }
    }
    sort_by_kind(&mut ranges);
    let moved_body: String = ranges.iter()
        .flat_map(|&(start, end, _, _)| &source_lines[start..=end])
        .cloned().collect::<Vec<_>>().join("\n");
    let (inner_attrs, use_lines) = filter_header(&source_content, &moved_body);
    let new_file_content = build_file_content(&inner_attrs, &use_lines, &ranges, &source_lines);
    let target_path_obj = std::path::Path::new(&to);
    let module_name = target_path_obj
        .file_stem().and_then(|s| s.to_str())
        .context("Cannot extract module name from --to path")?;
    let reexport_line = compute_reexport(source_path, &to, &symbol_names);
    let mod_decl = format!("mod {module_name};");
    let source_dir = source_path.parent().context("source file has no parent directory")?;
    if dry_run {
        print_dry_run(&to, &new_file_content, &locs[0].rel_path, &reexport_line, source_dir, &mod_decl, &ranges);
        return Ok(());
    }
    let root = rude_util::find_project_root(db)
        .context("Cannot determine project root from DB path")?;
    let target_abs = root.join(&to);
    if target_abs.exists() {
        bail!("Target file already exists: {} (use a different --to path)", target_abs.display());
    }
    if let Some(parent) = target_abs.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directories for {}", parent.display()))?;
    }
    std::fs::write(&target_abs, &new_file_content)
        .with_context(|| format!("Failed to write {}", target_abs.display()))?;
    eprintln!("Created {} ({} line(s))", to, new_file_content.lines().count());
    let ops: Vec<(&str, Op)> = symbol_names.iter().map(|&s| (s, Op::Delete)).collect();
    super::apply_edits(&ops, None)?;
    if !reexport_line.is_empty() {
        insert_reexport(source_path, &reexport_line)?;
        eprintln!("Inserted re-export: {}", reexport_line);
    }
    let mod_file = find_mod_file(source_dir);
    if let Some(mod_file) = mod_file {
        insert_mod_decl(&mod_file, &mod_decl, module_name)?;
        eprintln!("Added '{}' to {}", mod_decl, mod_file.display());
    }
    Ok(())
}

pub fn split_module(file: String, targets: Vec<String>, dry_run: bool) -> Result<()> {
    let db = crate::db();
    let root = rude_util::find_project_root(db).context("Cannot determine project root")?;
    let graph = crate::commands::intel::load_or_build_graph()?;
    // parse targets: "mir.rs:sym1,sym2" → (filename, vec![sym names])
    let parsed: Vec<(String, Vec<String>)> = targets.iter().map(|t| {
        let (file_part, syms) = t.split_once(':')
            .with_context(|| format!("Invalid target format '{t}', expected 'file.rs:sym1,sym2'"))?;
        let sym_list: Vec<String> = syms.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if sym_list.is_empty() { bail!("No symbols specified for {file_part}"); }
        Ok((file_part.to_string(), sym_list))
    }).collect::<Result<_>>()?;
    // find source file
    let src_chunk = graph.chunks.iter()
        .find(|c| c.file.ends_with(&file) || file.ends_with(&c.file))
        .with_context(|| format!("No file matching '{file}' in DB"))?;
    let abs_src = super::file::resolve_abs_path(db, &src_chunk.file)?;
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
    // resolve all symbol locations using the shared graph
    let file_hint = Some(src_chunk.file.as_str());
    let mut all_locs: Vec<(usize, SymbolLocation)> = Vec::new(); // (target_idx, loc)
    for (ti, (_, syms)) in parsed.iter().enumerate() {
        for sym in syms {
            let loc = locate_symbol_in(&graph, db, sym, file_hint)
                .with_context(|| format!("Symbol '{sym}' not found in {file}"))?;
            all_locs.push((ti, loc));
        }
    }
    // collect inner attrs from source header
    let inner_attrs: Vec<String> = source_content.lines()
        .take_while(|l| is_header_line(l.trim()))
        .filter(|l| l.trim().starts_with("#!["))
        .map(|l| l.to_string())
        .collect();
    // build each target file
    struct TargetFile { rel_path: String, content: String, symbols: Vec<String> }
    let mut target_files: Vec<TargetFile> = Vec::new();
    let mut all_moved_ranges: Vec<(usize, usize)> = Vec::new();
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
        let moved_body: String = ranges.iter()
            .flat_map(|&(start, end, _, _)| &source_lines[start..=end])
            .cloned().collect::<Vec<_>>().join("\n");
        let (_, use_lines) = filter_header(&source_content, &moved_body);
        let content = build_file_content(&inner_attrs, &use_lines, &ranges, &source_lines);
        for &(start, end, _, _) in &ranges {
            all_moved_ranges.push((start, end));
        }
        target_files.push(TargetFile {
            rel_path: target_name.clone(),
            content,
            symbols: syms.clone(),
        });
    }
    // compute what remains in mod.rs after removal
    let remaining_symbols: Vec<String> = {
        let moved_set: std::collections::HashSet<(usize, usize)> = all_moved_ranges.iter().copied().collect();
        graph.chunks.iter()
            .filter(|c| c.file == src_chunk.file)
            .filter(|c| {
                if let Some((s, e)) = c.lines {
                    !moved_set.iter().any(|&(ms, me)| s.saturating_sub(1) >= ms && e.saturating_sub(1) <= me)
                } else { true }
            })
            .filter(|c| c.kind == "function" && !c.name.contains("::"))
            .map(|c| c.name.rsplit("::").next().unwrap_or(&c.name).to_string())
            .collect()
    };
    if dry_run {
        eprintln!("=== DRY RUN: split-module {} ===\n", file);
        if !is_already_dir { eprintln!("Step 1: Rename {} → {}/mod.rs\n", file, stem); }
        for tf in &target_files {
            eprintln!("--- {} ({} line(s), {} symbols) ---", tf.rel_path, tf.content.lines().count(), tf.symbols.len());
            for (i, line) in tf.content.lines().enumerate() { eprintln!("{:>4}| {line}", i + 1); }
            eprintln!();
        }
        eprintln!("--- mod.rs (after cleanup) ---");
        for tf in &target_files {
            let mod_name = Path::new(&tf.rel_path).file_stem().and_then(|s| s.to_str()).unwrap_or("");
            eprintln!("  mod {mod_name};");
        }
        for tf in &target_files {
            let mod_name = Path::new(&tf.rel_path).file_stem().and_then(|s| s.to_str()).unwrap_or("");
            eprintln!("  pub use {mod_name}::{{{}}};", tf.symbols.join(", "));
        }
        if !remaining_symbols.is_empty() {
            eprintln!("\n  Remaining symbols in mod.rs: {}", remaining_symbols.join(", "));
        }
        return Ok(());
    }
    // execute: to_dir if needed
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
    // remove moved symbols from mod.rs (by line ranges, descending)
    all_moved_ranges.sort_by(|a, b| b.0.cmp(&a.0));
    super::file::splice_file(&mod_rs_path, |lines| {
        for &(start, end) in &all_moved_ranges {
            let drain_end = end.min(lines.len().saturating_sub(1));
            if start <= drain_end {
                lines.drain(start..=drain_end);
            }
        }
    })?;
    // rebuild mod.rs: keep only inner attrs, mod decls, pub use re-exports
    locked_edit(&mod_rs_path, |content| {
        let mut result: Vec<String> = Vec::new();
        // inner attrs
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with("#![") { result.push(line.to_string()); }
        }
        if !result.is_empty() { result.push(String::new()); }
        // existing mod declarations
        for line in content.lines() {
            let t = line.trim();
            if (t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("pub(crate) mod ")) && t.ends_with(';') {
                if !result.contains(&line.to_string()) { result.push(line.to_string()); }
            }
        }
        // new mod declarations
        for tf in &target_files {
            let mod_name = Path::new(&tf.rel_path).file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let decl = format!("mod {mod_name};");
            if !result.iter().any(|l| l.trim() == decl || l.trim() == format!("pub mod {mod_name};")) {
                result.push(decl);
            }
        }
        result.push(String::new());
        // existing pub use re-exports (keep)
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with("pub use ") || t.starts_with("pub(crate) use ") {
                result.push(line.to_string());
            }
        }
        // new pub use re-exports for moved symbols
        for tf in &target_files {
            let mod_name = Path::new(&tf.rel_path).file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let pub_syms: Vec<&str> = tf.symbols.iter()
                .map(|s| s.as_str()).collect();
            if pub_syms.len() == 1 {
                result.push(format!("pub use {mod_name}::{};", pub_syms[0]));
            } else if !pub_syms.is_empty() {
                result.push(format!("pub use {mod_name}::{{{}}};", pub_syms.join(", ")));
            }
        }
        // remaining function code (not moved)
        let remaining: Vec<&str> = content.lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with("#![") && !t.starts_with("use ")
                    && !t.starts_with("pub use ") && !t.starts_with("pub(crate) use ")
                    && !t.starts_with("mod ") && !t.starts_with("pub mod ") && !t.starts_with("pub(crate) mod ")
                    && !t.starts_with("//")
            })
            .collect();
        if !remaining.is_empty() {
            result.push(String::new());
            for &line in &remaining { result.push(line.to_string()); }
        }
        result.push(String::new());
        Ok(result.join("\n"))
    })?;
    eprintln!("Updated {}/mod.rs", stem);
    Ok(())
}

fn sort_by_kind(ranges: &mut [(usize, usize, &str, &str)]) {
    let kind_order = |k: &str| match k { "struct" | "enum" | "trait" => 0, "impl" => 1, _ => 2 };
    ranges.sort_by(|a, b| kind_order(a.3).cmp(&kind_order(b.3)).then(a.0.cmp(&b.0)));
}

fn build_file_content(inner_attrs: &[String], use_lines: &[String], ranges: &[(usize, usize, &str, &str)], source_lines: &[&str]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !inner_attrs.is_empty() { parts.extend(inner_attrs.iter().cloned()); parts.push(String::new()); }
    if !use_lines.is_empty() { parts.extend(use_lines.iter().cloned()); parts.push(String::new()); }
    for (i, &(start, end, _, _)) in ranges.iter().enumerate() {
        if i > 0 { parts.push(String::new()); }
        parts.extend(source_lines[start..=end].iter().map(|l| l.to_string()));
    }
    parts.join("\n") + "\n"
}

fn compute_reexport(source_path: &Path, to: &str, symbols: &[&str]) -> String {
    let source_name = source_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let target = std::path::Path::new(to);
    let target_stem = target.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if let Some(parent) = target.parent() {
        let parent_name = parent.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if parent_name == source_name || source_name == "mod" {
            if symbols.len() == 1 {
                return format!("pub use {target_stem}::{};", symbols[0]);
            }
            return format!("pub use {target_stem}::{{{}}};", symbols.join(", "));
        }
    }
    let module_name = target_stem;
    if symbols.len() == 1 {
        format!("pub use crate::{module_name}::{};", symbols[0])
    } else {
        format!("pub use crate::{module_name}::{{{}}};", symbols.join(", "))
    }
}

fn is_header_line(t: &str) -> bool {
    t.is_empty() || t.starts_with("use ") || t.starts_with("pub use ")
        || t.starts_with("//") || t.starts_with("#[") || t.starts_with("#![")
        || t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("extern ")
}

fn filter_header(source: &str, body: &str) -> (Vec<String>, Vec<String>) {
    let mut inner_attrs = Vec::new();
    let mut use_lines = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if !is_header_line(trimmed) { break; }
        if trimmed.starts_with("#![") {
            inner_attrs.push(line.to_string());
        } else if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
            let idents = super::imports::extract_use_idents(trimmed);
            if idents.iter().any(|id| super::imports::ident_used_in(body, id)) {
                use_lines.push(line.to_string());
            }
        }
    }
    (inner_attrs, use_lines)
}

fn find_mod_file(dir: &Path) -> Option<PathBuf> {
    [dir.join("lib.rs"), dir.join("mod.rs"), dir.join("main.rs")]
        .into_iter().find(|p| p.exists())
}

fn print_dry_run(to: &str, content: &str, rel: &str, reexport: &str, dir: &Path, mod_decl: &str, ranges: &[(usize, usize, &str, &str)]) {
    eprintln!("=== DRY RUN ===\n--- New file: {to} ---");
    for (i, line) in content.lines().enumerate() { eprintln!("{:>4}| {line}", i + 1); }
    if !reexport.is_empty() { eprintln!("\n--- Re-export in {rel} ---\n  {reexport}"); }
    if let Some(mf) = find_mod_file(dir) { eprintln!("--- {}: {mod_decl} ---", mf.display()); }
    eprintln!("\n--- Deletions from {rel} ---");
    for &(s, e, name, _) in ranges { eprintln!("  Delete '{name}' L{}-{}", s + 1, e + 1); }
}

fn insert_line_after(path: &Path, matcher: impl Fn(&str) -> bool, line: &str, skip_if: Option<&str>) -> anyhow::Result<()> {
    locked_edit(path, |content| {
        if let Some(check) = skip_if {
            if content.lines().any(|l| l.trim() == check) { return Ok(content.to_string()); }
        }
        let lines: Vec<&str> = content.lines().collect();
        let pos = lines.iter().rposition(|l| !l.starts_with(' ') && !l.starts_with('\t') && matcher(l.trim())).map(|i| i + 1).unwrap_or(0);
        let mut result: Vec<String> = Vec::with_capacity(lines.len() + 2);
        for (i, &l) in lines.iter().enumerate() {
            result.push(l.to_string());
            if i + 1 == pos { result.push(line.to_string()); }
        }
        if pos == 0 { result.insert(0, line.to_string()); if !lines.is_empty() { result.insert(1, String::new()); } }
        let trailing = content.ends_with('\n');
        Ok(if trailing { result.join("\n") + "\n" } else { result.join("\n") })
    })
}

fn insert_reexport(path: &Path, reexport_line: &str) -> anyhow::Result<()> {
    insert_line_after(path, |t| t.starts_with("use ") || t.starts_with("pub use "), reexport_line, None)
}

fn insert_mod_decl(path: &Path, mod_decl: &str, module_name: &str) -> anyhow::Result<()> {
    let check = format!("mod {module_name};");
    insert_line_after(path,
        |t| (t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("pub(crate) mod ")) && t.ends_with(';'),
        mod_decl, Some(&check))
}
