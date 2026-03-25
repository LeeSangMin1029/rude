use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

use rude_intel::loader::load_chunks;
use rude_intel::parse::ParsedChunk;

// ── Types ───────────────────────────────────────────────────────────

pub(crate) struct SymbolLocation {
    pub(crate) abs_path: PathBuf,
    pub(crate) rel_path: String,
    pub(crate) start_line: usize, // 0-based
    pub(crate) end_line: usize,   // 0-based inclusive
}

pub enum Op<'a> { Replace(&'a str), Before(&'a str), After(&'a str), Delete }

// ── Core: symbol-based editing ──────────────────────────────────────

/// All symbol edits: locate → splice → write. One locked pass, no line drift.
pub fn apply_edits(ops: &[(&str, Op)], file: Option<&str>) -> Result<()> {
    let db = crate::db();
    if ops.is_empty() { return Ok(()); }

    let deletes: Vec<&str> = ops.iter()
        .filter(|(_, op)| matches!(op, Op::Delete))
        .map(|(s, _)| *s).collect();
    if !deletes.is_empty() { warn_callers(&deletes); }

    let mut edits: Vec<(usize, usize, &str, &Op)> = ops.iter()
        .map(|(sym, op)| {
            let loc = locate_symbol(db, sym, file).unwrap();
            (loc.start_line, loc.end_line, *sym, op)
        }).collect();
    edits.sort_by(|a, b| b.0.cmp(&a.0));

    let first = locate_symbol(db, ops[0].0, file)?;
    splice_file(&first.abs_path, |lines| {
        for &(start, end, sym, op) in &edits {
            let (drain, repl) = op_to_splice(start, end, op, lines.len());
            let owned: Vec<String> = repl.into_iter().map(String::from).collect();
            lines.splice(drain, owned);
            eprintln!("  {} {sym} in {}", op_label(op, start, end), first.rel_path);
        }
    })
}

// ── Core: line-based editing ────────────────────────────────────────

/// Insert content at a line number (1-based).
pub fn insert_at(file: String, line: usize, body: String) -> Result<()> {
    check_line(line)?;
    let (abs, rel) = resolve_path(crate::db(), &file)?;
    splice_file(&abs, |lines| {
        let idx = (line - 1).min(lines.len());
        let bl: Vec<String> = body.trim_end().lines().map(String::from).collect();
        let n = bl.len();
        lines.splice(idx..idx, bl);
        eprintln!("  Inserted {n} line(s) at L{line} in {rel}");
    })
}

/// Delete a line range (1-based inclusive).
pub fn delete_lines(file: String, start: usize, end: usize) -> Result<()> {
    check_range(start, end)?;
    let (abs, rel) = resolve_path(crate::db(), &file)?;
    splice_file(&abs, |lines| {
        let mut after = end.min(lines.len());
        while after < lines.len() && lines[after].trim().is_empty() { after += 1; }
        lines.splice((start - 1)..after, Vec::<String>::new());
        eprintln!("  Deleted L{start}-{end} from {rel}");
    })
}

/// Replace a line range (1-based inclusive) with new content.
pub fn replace_lines(file: String, start: usize, end: usize, body: String) -> Result<()> {
    check_range(start, end)?;
    let (abs, rel) = resolve_path(crate::db(), &file)?;
    splice_file(&abs, |lines| {
        let bl: Vec<String> = body.trim_end().lines().map(String::from).collect();
        lines.splice((start - 1)..end.min(lines.len()), bl);
        eprintln!("  Replaced L{start}-{end} in {rel}");
    })
}

/// Create a new file at a project-relative path.
pub fn create_file(file: String, body: String) -> Result<()> {
    let root = rude_intel::helpers::find_project_root(crate::db())
        .context("Cannot determine project root")?;
    let path = root.join(&file);
    if path.exists() { bail!("File already exists: {}", path.display()); }
    if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
    std::fs::write(&path, body.trim_end().to_owned() + "\n")?;
    eprintln!("  Created {file}");
    Ok(())
}

// ── Splice engine ───────────────────────────────────────────────────

/// Locked read-modify-write. Returns lines for splicing.
fn splice_file(path: &Path, f: impl FnOnce(&mut Vec<String>)) -> Result<()> {
    locked_edit(path, |content| {
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        let trailing = content.ends_with('\n');
        f(&mut lines);
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        Ok(join_lines(&refs, trailing))
    })
}

/// Compute (drain_range, replacement) for a symbol Op.
fn op_to_splice<'a>(start: usize, end: usize, op: &'a Op, len: usize) -> (std::ops::Range<usize>, Vec<&'a str>) {
    match op {
        Op::Replace(b) => {
            (start..(end + 1).min(len), b.trim_end().lines().collect())
        }
        Op::Before(b) => {
            let mut r: Vec<&str> = b.trim_end().lines().collect();
            r.push("");
            (start..start, r)
        }
        Op::After(b) => {
            let pos = (end + 1).min(len);
            let mut r = vec![""];
            r.extend(b.trim_end().lines());
            (pos..pos, r)
        }
        Op::Delete => {
            (start..(end + 1).min(len), vec![])
        }
    }
}

fn op_label(op: &Op, start: usize, end: usize) -> String {
    match op {
        Op::Replace(_) => format!("Replaced (L{}-{})", start + 1, end + 1),
        Op::Before(_) => format!("Inserted before (L{})", start + 1),
        Op::After(_) => format!("Inserted after (L{})", end + 1),
        Op::Delete => format!("Deleted (L{}-{})", start + 1, end + 1),
    }
}

// ── Shared utilities ────────────────────────────────────────────────

pub(crate) fn locate_symbol(db: &Path, symbol: &str, file_hint: Option<&str>) -> Result<SymbolLocation> {
    let chunks = load_chunks(db)?;

    let mut idx: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
    for (i, c) in chunks.iter().enumerate() {
        idx.entry(&c.name).or_default().push(i);
        if let Some(s) = c.name.rsplit("::").next() {
            if s != c.name { idx.entry(s).or_default().push(i); }
        }
    }

    let mut candidates: Vec<usize> = idx.get(symbol).cloned().unwrap_or_default();
    // Fallback: intermediate path (e.g. "MirEdgeMap::from_dir")
    if candidates.is_empty() && symbol.contains("::") {
        for (i, c) in chunks.iter().enumerate() {
            if c.name.ends_with(&format!("::{symbol}")) { candidates.push(i); }
        }
    }
    let candidates: Vec<&ParsedChunk> = candidates.into_iter()
        .map(|i| &chunks[i])
        .filter(|c| {
            let nm = c.name == symbol || c.name.ends_with(&format!("::{symbol}"));
            let fm = file_hint.is_none_or(|f| c.file.ends_with(f));
            nm && fm
        }).collect();

    if candidates.is_empty() { bail!("Symbol '{symbol}' not found"); }
    if candidates.len() > 1 {
        let locs: Vec<String> = candidates.iter()
            .map(|c| format!("  {} [{}] {}:{}", c.name, c.kind,
                c.file, c.lines.map_or("?".into(), |(s, e)| format!("{s}-{e}")))).collect();
        bail!("Ambiguous '{symbol}' — {} matches:\n{}", candidates.len(), locs.join("\n"));
    }

    let chunk = candidates[0];
    let (start_1, end_1) = chunk.lines.context("No line range")?;

    let abs_path = resolve_abs_path(db, &chunk.file)?;
    let content = std::fs::read_to_string(&abs_path)?;
    let lines: Vec<&str> = content.lines().collect();

    let mut start = start_1.saturating_sub(1);
    let mut end = end_1.saturating_sub(1);
    if end >= lines.len() { bail!("L{start_1}-{end_1} exceeds file ({} lines)", lines.len()); }

    // Extend upward: doc comments + attributes (not #[test])
    while start > 0 {
        let p = lines[start - 1].trim();
        if p.starts_with("///") || p.starts_with("//!")
            || (p.starts_with("#[") && !p.starts_with("#[test") && !p.starts_with("#[cfg(test"))
            || p.starts_with("#![") || p.starts_with("/** ") || p.starts_with("* ") || p == "*/"
        { start -= 1; } else { break; }
    }
    // Extend downward: find matching closing brace
    if (start..=end).any(|i| lines[i].contains('{'))
        && !(start..=end).any(|i| lines[i].contains('}'))
    {
        let mut depth: i32 = 0;
        for i in start..lines.len() {
            for ch in lines[i].chars() {
                if ch == '{' { depth += 1; }
                if ch == '}' { depth -= 1; }
            }
            if i > end && depth <= 0 { end = i; break; }
        }
    }

    let rel = relative_display(db, &chunk.file);
    Ok(SymbolLocation { abs_path, rel_path: rel, start_line: start, end_line: end })
}

pub(crate) fn locked_edit<F: FnOnce(&str) -> Result<String>>(path: &Path, f: F) -> Result<()> {
    let lock_path = path.with_extension("lock");
    let lock = File::create(&lock_path)?;
    lock.lock_exclusive()?;
    let content = std::fs::read_to_string(path)?;
    let new = f(&content)?;
    std::fs::write(path, new)?;
    lock.unlock()?;
    let _ = std::fs::remove_file(&lock_path);
    Ok(())
}

pub(crate) fn join_lines(lines: &[&str], trailing_nl: bool) -> String {
    if trailing_nl { lines.join("\n") + "\n" } else { lines.join("\n") }
}

fn warn_callers(symbols: &[&str]) {
    let Ok((graph, _)) = crate::commands::intel::load_or_build_graph_with_chunks() else { return };
    for &sym in symbols {
        let idxs: Vec<usize> = graph.names.iter().enumerate()
            .filter(|(_, n)| *n == sym || n.ends_with(&format!("::{sym}")))
            .map(|(i, _)| i).collect();
        for &i in &idxs {
            let callers: Vec<_> = graph.callers[i].iter()
                .filter(|&&c| !graph.is_test[c as usize] && !idxs.contains(&(c as usize))).collect();
            if !callers.is_empty() {
                eprintln!("  warning: {sym} has {} caller(s):", callers.len());
                for &&c in &callers { eprintln!("    → {} ({})", graph.names[c as usize], graph.files[c as usize]); }
            }
        }
    }
}

fn resolve_abs_path(db: &Path, file: &str) -> Result<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let db_dir = db.parent().unwrap_or(Path::new("."));
    let p = PathBuf::from(file);
    if p.is_absolute() { return Ok(rude_db::strip_unc_prefix_path(&p)); }
    let try_cwd = cwd.join(file);
    if try_cwd.exists() { return Ok(try_cwd); }
    let try_db = db_dir.canonicalize().unwrap_or(db_dir.to_path_buf()).join(file);
    Ok(rude_db::strip_unc_prefix_path(&try_db))
}

fn resolve_path(db: &Path, file: &str) -> Result<(PathBuf, String)> {
    let abs = resolve_abs_path(db, file)?;
    if !abs.exists() { bail!("File not found: {}", abs.display()); }
    let rel = relative_display(db, file);
    Ok((abs, rel))
}

fn relative_display(db: &Path, file: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = if cwd.join(file).exists() { cwd } else {
        db.parent().unwrap_or(Path::new(".")).canonicalize().unwrap_or_default()
    };
    let norm = file.replace('\\', "/");
    let root_s = rude_db::strip_unc_prefix(&root.to_string_lossy()).replace('\\', "/");
    norm.strip_prefix(&format!("{root_s}/")).unwrap_or(&norm).to_string()
}

fn check_line(line: usize) -> Result<()> {
    if line == 0 { bail!("--line must be >= 1"); }
    Ok(())
}

fn check_range(start: usize, end: usize) -> Result<()> {
    if start == 0 || end == 0 { bail!("--start/--end must be >= 1"); }
    if start > end { bail!("--start ({start}) > --end ({end})"); }
    Ok(())
}

// ── Split command ────────────────────────────────────────────────────

fn is_header_line(t: &str) -> bool {
    t.is_empty()
        || t.starts_with("use ")
        || t.starts_with("pub use ")
        || t.starts_with("//")
        || t.starts_with("#[")
        || t.starts_with("#![")
        || t.starts_with("mod ")
        || t.starts_with("pub mod ")
        || t.starts_with("extern ")
}

/// Extract use/import lines from the top of a file.
fn extract_use_lines(content: &str) -> Vec<String> {
    let mut uses = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
            uses.push(line.to_string());
        }
        if !is_header_line(trimmed) { break; }
    }
    uses
}

fn print_dry_run(
    to: &str,
    new_file_content: &str,
    rel_path: &str,
    reexport_line: &str,
    source_dir: &std::path::Path,
    mod_decl: &str,
    ranges: &[(usize, usize, &str)],
) {
    eprintln!("=== DRY RUN — no files will be modified ===");
    eprintln!();
    eprintln!("--- New file: {} ---", to);
    for (i, line) in new_file_content.lines().enumerate() {
        eprintln!("{:>4}| {}", i + 1, line);
    }
    eprintln!();
    eprintln!("--- Re-export in {} ---", rel_path);
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
    eprintln!("--- Deletions from {} ---", rel_path);
    for &(start, end, name) in ranges {
        eprintln!("  Delete '{}' L{}-{}", name, start + 1, end + 1);
    }
}

fn insert_reexport(path: &std::path::Path, reexport_line: &str) -> Result<()> {
    locked_edit(path, |content| {
        let lines: Vec<&str> = content.lines().collect();
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
            if i + 1 == use_end { result.push(reexport_line.to_string()); }
        }
        if use_end == 0 {
            result.insert(0, reexport_line.to_string());
            if !lines.is_empty() { result.insert(1, String::new()); }
        }

        let trailing = content.ends_with('\n');
        Ok(if trailing { result.join("\n") + "\n" } else { result.join("\n") })
    })
}

fn insert_mod_decl(path: &std::path::Path, mod_decl: &str, module_name: &str) -> Result<()> {
    locked_edit(path, |content| {
        let already_declared = content.lines().any(|l| {
            let t = l.trim();
            t == format!("mod {module_name};")
                || t == format!("pub mod {module_name};")
                || t == format!("pub(crate) mod {module_name};")
        });
        if already_declared { return Ok(content.to_string()); }

        let lines: Vec<&str> = content.lines().collect();
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
            if i + 1 == mod_end { result.push(mod_decl.to_string()); }
        }
        if mod_end == 0 { result.insert(0, mod_decl.to_string()); }

        let trailing = content.ends_with('\n');
        Ok(if trailing { result.join("\n") + "\n" } else { result.join("\n") })
    })
}

/// Split symbols from a source file into a new module file.
pub fn split(symbols: String, to: String, dry_run: bool) -> Result<()> {
    let db = crate::db();
    let symbol_names: Vec<&str> = symbols.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if symbol_names.is_empty() { bail!("--symbols must contain at least one symbol name"); }

    // Locate all symbols — all must be in the same source file.
    let locs: Vec<SymbolLocation> = symbol_names.iter()
        .map(|&s| locate_symbol(&db, s, None))
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

    // Build new file: use lines + symbol blocks.
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

    // Write new file.
    let root = rude_intel::helpers::find_project_root(&db)
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

    // Delete symbols from source.
    let ops: Vec<(&str, Op)> = symbol_names.iter().map(|&s| (s, Op::Delete)).collect();
    apply_edits(&ops, None)?;

    // Insert re-export and mod declaration.
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
