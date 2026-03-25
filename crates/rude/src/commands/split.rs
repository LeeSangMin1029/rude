//! Split symbols from a source file into a new module file.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::edit::{locate_symbol, locked_edit};
use rude_intel::helpers::find_project_root;

/// Extract use/import lines from the top of a file.
fn extract_use_lines(content: &str) -> Vec<String> {
    let mut uses = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        // Collect `use ...;` lines (including multi-line `use { ... };`)
        if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
            uses.push(line.to_string());
        }
        // Stop once we hit a non-use, non-blank, non-comment, non-attribute line
        // that isn't a module-level item
        if !trimmed.is_empty()
            && !trimmed.starts_with("use ")
            && !trimmed.starts_with("pub use ")
            && !trimmed.starts_with("//")
            && !trimmed.starts_with("#[")
            && !trimmed.starts_with("#![")
            && !trimmed.starts_with("mod ")
            && !trimmed.starts_with("pub mod ")
            && !trimmed.starts_with("extern ")
        {
            break;
        }
    }
    uses
}

/// Run the split command.
pub fn run(db: PathBuf, symbols: String, to: String, dry_run: bool) -> Result<()> {
    let symbol_names: Vec<&str> = symbols.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if symbol_names.is_empty() {
        bail!("--symbols must contain at least one symbol name");
    }

    // Locate all symbols and collect their info.
    // All symbols must come from the same source file.
    struct SymbolInfo {
        start_line: usize,
        end_line: usize,
        abs_path: PathBuf,
        rel_path: String,
    }

    let mut infos: Vec<SymbolInfo> = Vec::new();
    for &sym in &symbol_names {
        let loc = locate_symbol(&db, sym, None)?;
        infos.push(SymbolInfo {
            start_line: loc.start_line,
            end_line: loc.end_line,
            abs_path: loc.abs_path,
            rel_path: loc.rel_path,
        });
    }

    // Verify all symbols are from the same file.
    let source_path = &infos[0].abs_path;
    for info in &infos[1..] {
        if info.abs_path != *source_path {
            bail!(
                "All symbols must be in the same file. Found '{}' and '{}'",
                infos[0].rel_path,
                info.rel_path,
            );
        }
    }

    let source_content = std::fs::read_to_string(source_path)
        .with_context(|| format!("Failed to read {}", source_path.display()))?;
    let source_lines: Vec<&str> = source_content.lines().collect();

    // Extract use lines from source.
    let use_lines = extract_use_lines(&source_content);

    // Extract symbol code blocks (sorted by start line for consistent output).
    let mut ranges: Vec<(usize, usize, &str)> = infos
        .iter()
        .zip(symbol_names.iter())
        .map(|(info, &name)| (info.start_line, info.end_line, name))
        .collect();
    ranges.sort_by_key(|r| r.0);

    // Check for overlapping ranges.
    for w in ranges.windows(2) {
        if w[0].1 >= w[1].0 {
            bail!(
                "Overlapping symbols: '{}' (L{}-{}) and '{}' (L{}-{})",
                w[0].2, w[0].0 + 1, w[0].1 + 1,
                w[1].2, w[1].0 + 1, w[1].1 + 1,
            );
        }
    }

    // Build the new file content: use lines + blank line + symbol code blocks.
    let mut new_file_parts: Vec<String> = Vec::new();
    if !use_lines.is_empty() {
        for u in &use_lines {
            new_file_parts.push(u.clone());
        }
        new_file_parts.push(String::new());
    }

    for (i, &(start, end, _name)) in ranges.iter().enumerate() {
        if i > 0 {
            new_file_parts.push(String::new());
        }
        for line_idx in start..=end {
            new_file_parts.push(source_lines[line_idx].to_string());
        }
    }

    let new_file_content = new_file_parts.join("\n") + "\n";

    // Compute module name and re-export line.
    let target_pathbuf = std::path::Path::new(&to);
    let module_name = target_pathbuf
        .file_stem()
        .and_then(|s| s.to_str())
        .context("Cannot extract module name from --to path")?;
    let symbols_list = symbol_names.join(", ");
    let reexport_line = format!("pub use {module_name}::{{{symbols_list}}};");
    let mod_decl = format!("pub mod {module_name};");

    // Find the parent directory of the source file for lib.rs / mod.rs lookup.
    let source_dir = source_path.parent().context("source file has no parent directory")?;

    if dry_run {
        eprintln!("=== DRY RUN — no files will be modified ===");
        eprintln!();
        eprintln!("--- New file: {} ---", to);
        for (i, line) in new_file_content.lines().enumerate() {
            eprintln!("{:>4}| {}", i + 1, line);
        }
        eprintln!();
        eprintln!("--- Re-export in {} ---", infos[0].rel_path);
        eprintln!("  {}", reexport_line);
        eprintln!();
        eprintln!("--- Module declaration ---");
        let lib_rs = source_dir.join("lib.rs");
        let mod_rs = source_dir.join("mod.rs");
        if lib_rs.exists() {
            eprintln!("  {} → {}", lib_rs.display(), mod_decl);
        } else if mod_rs.exists() {
            eprintln!("  {} → {}", mod_rs.display(), mod_decl);
        } else {
            eprintln!("  (no lib.rs or mod.rs found in {})", source_dir.display());
        }
        eprintln!();
        eprintln!("--- Deletions from {} ---", infos[0].rel_path);
        for &(start, end, name) in &ranges {
            eprintln!("  Delete '{}' L{}-{}", name, start + 1, end + 1);
        }
        return Ok(());
    }

    // Resolve target file path.
    let root = find_project_root(&db)
        .context("Cannot determine project root from DB path")?;
    let target_path = root.join(&to);

    if target_path.exists() {
        bail!("Target file already exists: {} (use a different --to path)", target_path.display());
    }

    // Create parent directories if needed.
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directories for {}", parent.display()))?;
    }

    // Write the new file.
    std::fs::write(&target_path, &new_file_content)
        .with_context(|| format!("Failed to write {}", target_path.display()))?;
    eprintln!("Created {} ({} line(s))", to, new_file_content.lines().count());

    // Delete symbols from source using shared delete_symbols logic.
    super::edit::delete_symbols(db.clone(), &symbol_names, None)?;

    // ── Insert `pub use` re-export into the source file ─────────────
    locked_edit(source_path, |content| {
        let lines: Vec<&str> = content.lines().collect();
        // Find the last `use ...` line position (0-based index).
        let use_end = lines
            .iter()
            .rposition(|l| {
                let t = l.trim();
                t.starts_with("use ") || t.starts_with("pub use ")
            })
            .map(|i| i + 1)
            .unwrap_or(0);

        let mut result: Vec<String> = Vec::with_capacity(lines.len() + 2);
        for (i, &line) in lines.iter().enumerate() {
            result.push(line.to_string());
            if i + 1 == use_end {
                result.push(reexport_line.clone());
            }
        }
        // If there were no use lines, insert at the top.
        if use_end == 0 {
            result.insert(0, reexport_line.clone());
            // Add a blank line after the re-export if the file is non-empty.
            if !lines.is_empty() {
                result.insert(1, String::new());
            }
        }

        let trailing = content.ends_with('\n');
        let joined = if trailing {
            result.join("\n") + "\n"
        } else {
            result.join("\n")
        };
        Ok(joined)
    })?;
    eprintln!("Inserted re-export: {}", reexport_line);

    // ── Add `pub mod` to lib.rs / mod.rs ────────────────────────────
    let lib_rs = source_dir.join("lib.rs");
    let mod_rs_path = source_dir.join("mod.rs");
    let mod_file = if lib_rs.exists() {
        Some(lib_rs)
    } else if mod_rs_path.exists() {
        Some(mod_rs_path)
    } else {
        None
    };

    if let Some(mod_file) = mod_file {
        locked_edit(&mod_file, |content| {
            // Check if the mod declaration already exists.
            let already_declared = content.lines().any(|l| {
                let t = l.trim();
                t == format!("mod {module_name};")
                    || t == format!("pub mod {module_name};")
                    || t == format!("pub(crate) mod {module_name};")
            });

            if already_declared {
                return Ok(content.to_string());
            }

            let lines: Vec<&str> = content.lines().collect();
            // Find the last `mod ...;` or `pub mod ...;` line.
            let mod_end = lines
                .iter()
                .rposition(|l| {
                    let t = l.trim();
                    (t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("pub(crate) mod "))
                        && t.ends_with(';')
                })
                .map(|i| i + 1)
                .unwrap_or(0);

            let mut result: Vec<String> = Vec::with_capacity(lines.len() + 1);
            for (i, &line) in lines.iter().enumerate() {
                result.push(line.to_string());
                if i + 1 == mod_end {
                    result.push(mod_decl.clone());
                }
            }
            if mod_end == 0 {
                result.insert(0, mod_decl.clone());
            }

            let trailing = content.ends_with('\n');
            let joined = if trailing {
                result.join("\n") + "\n"
            } else {
                result.join("\n")
            };
            Ok(joined)
        })?;
        eprintln!("Added '{}' to {}", mod_decl, mod_file.display());
    } else {
        eprintln!("Warning: no lib.rs or mod.rs found in {}, skipping mod declaration", source_dir.display());
    }

    Ok(())
}
