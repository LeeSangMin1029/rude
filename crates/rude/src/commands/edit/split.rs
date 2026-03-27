use std::path::Path;
use anyhow::{Context, Result, bail};
use super::ops::Op;
use super::locate::{SymbolLocation, locate_symbol};
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
    let use_lines = extract_use_lines(&source_content);
    let mut ranges: Vec<(usize, usize, &str)> = locs.iter()
        .zip(symbol_names.iter())
        .map(|(loc, &name)| (loc.start_line, loc.end_line, name))
        .collect();
    ranges.sort_by_key(|r| r.0);
    for w in ranges.windows(2) {
        if w[0].1 >= w[1].0 {
            bail!("Overlapping symbols: '{}' (L{}-{}) and '{}' (L{}-{})",
                w[0].2, w[0].0 + 1, w[0].1 + 1, w[1].2, w[1].0 + 1, w[1].1 + 1);
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if !use_lines.is_empty() { parts.extend(use_lines); parts.push(String::new()); }
    for (i, &(start, end, _)) in ranges.iter().enumerate() {
        if i > 0 { parts.push(String::new()); }
        parts.extend(source_lines[start..=end].iter().map(|l| l.to_string()));
    }
    let new_file_content = parts.join("\n") + "\n";
    let module_name = std::path::Path::new(&to)
        .file_stem().and_then(|s| s.to_str())
        .context("Cannot extract module name from --to path")?;
    let reexport_line = format!("pub use {module_name}::{{{}}};", symbol_names.join(", "));
    let mod_decl = format!("pub mod {module_name};");
    let source_dir = source_path.parent().context("source file has no parent directory")?;
    if dry_run {
        print_dry_run(&to, &new_file_content, &locs[0].rel_path, &reexport_line, source_dir, &mod_decl, &ranges);
        return Ok(());
    }
    let root = rude_intel::helpers::find_project_root(db)
        .context("Cannot determine project root from DB path")?;
    let target_path = root.join(&to);
    if target_path.exists() {
        bail!("Target file already exists: {} (use a different --to path)", target_path.display());
    }
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directories for {}", parent.display()))?;
    }
    std::fs::write(&target_path, &new_file_content)
        .with_context(|| format!("Failed to write {}", target_path.display()))?;
    eprintln!("Created {} ({} line(s))", to, new_file_content.lines().count());
    let ops: Vec<(&str, Op)> = symbol_names.iter().map(|&s| (s, Op::Delete)).collect();
    super::apply_edits(&ops, None)?;
    insert_reexport(source_path, &reexport_line)?;
    eprintln!("Inserted re-export: {}", reexport_line);
    let mod_file = [source_dir.join("lib.rs"), source_dir.join("mod.rs")]
        .into_iter().find(|p| p.exists());
    if let Some(mod_file) = mod_file {
        insert_mod_decl(&mod_file, &mod_decl, module_name)?;
        eprintln!("Added '{}' to {}", mod_decl, mod_file.display());
    } else {
        eprintln!("Warning: no lib.rs or mod.rs found in {}, skipping mod declaration", source_dir.display());
    }
    Ok(())
}

fn is_header_line(t: &str) -> bool {
    t.is_empty() || t.starts_with("use ") || t.starts_with("pub use ")
        || t.starts_with("//") || t.starts_with("#[") || t.starts_with("#![")
        || t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("extern ")
}

fn extract_use_lines(content: &str) -> Vec<String> {
    let mut uses = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") { uses.push(line.to_string()); }
        if !is_header_line(trimmed) { break; }
    }
    uses
}

fn print_dry_run(to: &str, content: &str, rel: &str, reexport: &str, dir: &Path, mod_decl: &str, ranges: &[(usize, usize, &str)]) {
    eprintln!("=== DRY RUN ===\n--- New file: {to} ---");
    for (i, line) in content.lines().enumerate() { eprintln!("{:>4}| {line}", i + 1); }
    eprintln!("\n--- Re-export in {rel} ---\n  {reexport}");
    let lib = dir.join("lib.rs"); let mod_rs = dir.join("mod.rs");
    if lib.exists() { eprintln!("--- {}: {mod_decl} ---", lib.display()); }
    else if mod_rs.exists() { eprintln!("--- {}: {mod_decl} ---", mod_rs.display()); }
    eprintln!("\n--- Deletions from {rel} ---");
    for &(s, e, name) in ranges { eprintln!("  Delete '{name}' L{}-{}", s + 1, e + 1); }
}

fn insert_line_after(path: &Path, matcher: impl Fn(&str) -> bool, line: &str, skip_if: Option<&str>) -> anyhow::Result<()> {
    locked_edit(path, |content| {
        if let Some(check) = skip_if {
            if content.lines().any(|l| l.trim() == check) { return Ok(content.to_string()); }
        }
        let lines: Vec<&str> = content.lines().collect();
        let pos = lines.iter().rposition(|l| matcher(l.trim())).map(|i| i + 1).unwrap_or(0);
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
    let check = format!("pub mod {module_name};");
    insert_line_after(path,
        |t| (t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("pub(crate) mod ")) && t.ends_with(';'),
        mod_decl, Some(&check))
}
