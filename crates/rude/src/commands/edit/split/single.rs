use anyhow::{Context, Result, bail};
use std::path::Path;
use crate::commands::edit::locate::{SymbolLocation, locate_symbol};
use crate::commands::edit::ops::Op;
use super::{sort_by_kind, filter_header, build_file_content, find_mod_file, insert_reexport, insert_mod_decl};

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
    crate::commands::edit::apply_edits(&ops, None)?;
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

fn print_dry_run(to: &str, content: &str, rel: &str, reexport: &str, dir: &Path, mod_decl: &str, ranges: &[(usize, usize, &str, &str)]) {
    eprintln!("=== DRY RUN ===\n--- New file: {to} ---");
    for (i, line) in content.lines().enumerate() { eprintln!("{:>4}| {line}", i + 1); }
    if !reexport.is_empty() { eprintln!("\n--- Re-export in {rel} ---\n  {reexport}"); }
    if let Some(mf) = find_mod_file(dir) { eprintln!("--- {}: {mod_decl} ---", mf.display()); }
    eprintln!("\n--- Deletions from {rel} ---");
    for &(s, e, name, _) in ranges { eprintln!("  Delete '{name}' L{}-{}", s + 1, e + 1); }
}
