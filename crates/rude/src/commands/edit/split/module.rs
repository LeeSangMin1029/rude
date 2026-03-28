use std::path::Path;
use anyhow::{Context, Result, bail};
use crate::commands::edit::locate::{SymbolLocation, locate_symbol_in};
use crate::commands::edit::file::locked_edit;
use super::{sort_by_kind, is_header_line, filter_header, build_file_content};

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
    let file_hint = Some(src_chunk.file.as_str());
    let mut all_locs: Vec<(usize, SymbolLocation)> = Vec::new();
    for (ti, (_, syms)) in parsed.iter().enumerate() {
        for sym in syms {
            let loc = locate_symbol_in(&graph, db, sym, file_hint)
                .with_context(|| format!("Symbol '{sym}' not found in {file}"))?;
            all_locs.push((ti, loc));
        }
    }
    let inner_attrs: Vec<String> = source_content.lines()
        .take_while(|l| is_header_line(l.trim()))
        .filter(|l| l.trim().starts_with("#!["))
        .map(|l| l.to_string())
        .collect();
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
    if !is_already_dir {
        std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
        std::fs::rename(&abs_src, &mod_rs_path)
            .with_context(|| format!("Failed to rename {} → {}", abs_src.display(), mod_rs_path.display()))?;
        let rel_src = abs_src.strip_prefix(&root).unwrap_or(&abs_src);
        eprintln!("Renamed {} → {}/mod.rs", rel_src.display(), stem);
    }
    for tf in &target_files {
        let target_abs = dir.join(&tf.rel_path);
        if target_abs.exists() { bail!("Target file already exists: {}", target_abs.display()); }
        std::fs::write(&target_abs, &tf.content)?;
        eprintln!("Created {}/{} ({} line(s))", stem, tf.rel_path, tf.content.lines().count());
    }
    all_moved_ranges.sort_by(|a, b| b.0.cmp(&a.0));
    crate::commands::edit::file::splice_file(&mod_rs_path, |lines| {
        for &(start, end) in &all_moved_ranges {
            let drain_end = end.min(lines.len().saturating_sub(1));
            if start <= drain_end {
                lines.drain(start..=drain_end);
            }
        }
    })?;
    locked_edit(&mod_rs_path, |content| {
        let mut result: Vec<String> = Vec::new();
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with("#![") { result.push(line.to_string()); }
        }
        if !result.is_empty() { result.push(String::new()); }
        for line in content.lines() {
            let t = line.trim();
            if (t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("pub(crate) mod ")) && t.ends_with(';') {
                if !result.contains(&line.to_string()) { result.push(line.to_string()); }
            }
        }
        for tf in &target_files {
            let mod_name = Path::new(&tf.rel_path).file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let decl = format!("mod {mod_name};");
            if !result.iter().any(|l| l.trim() == decl || l.trim() == format!("pub mod {mod_name};")) {
                result.push(decl);
            }
        }
        result.push(String::new());
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with("pub use ") || t.starts_with("pub(crate) use ") {
                result.push(line.to_string());
            }
        }
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
        // remaining code stays in mod.rs (shared utils, types, etc.)
        let remaining_body: String = {
            let lines: Vec<&str> = content.lines().collect();
            let moved_set: std::collections::HashSet<(usize, usize)> = all_moved_ranges.iter().copied().collect();
            let mut body_lines: Vec<String> = Vec::new();
            let mut in_header = true;
            for (i, &line) in lines.iter().enumerate() {
                let t = line.trim();
                if in_header {
                    if t.starts_with("#![") || t.starts_with("use ") || t.starts_with("pub use ")
                        || t.starts_with("pub(crate) use ") || t.starts_with("mod ") || t.starts_with("pub mod ")
                        || t.starts_with("pub(crate) mod ") || t.starts_with("//") || t.is_empty()
                        || t.starts_with("extern ") || t.starts_with("#[") { continue; }
                    in_header = false;
                }
                let in_moved = moved_set.iter().any(|&(ms, me)| i >= ms && i <= me);
                if !in_moved { body_lines.push(line.to_string()); }
            }
            while body_lines.last().is_some_and(|l| l.trim().is_empty()) { body_lines.pop(); }
            while body_lines.first().is_some_and(|l| l.trim().is_empty()) { body_lines.remove(0); }
            body_lines.join("\n")
        };
        if !remaining_body.is_empty() {
            // add use statements needed by remaining code
            let remaining_uses = filter_header(content, &remaining_body).1;
            if !remaining_uses.is_empty() {
                result.push(String::new());
                result.extend(remaining_uses);
            }
            result.push(String::new());
            result.extend(remaining_body.lines().map(|l| l.to_string()));
        }
        result.push(String::new());
        Ok(result.join("\n"))
    })?;
    eprintln!("Updated {}/mod.rs", stem);
    Ok(())
}
