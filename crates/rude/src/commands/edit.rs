use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;


pub(crate) struct SymbolLocation {
    pub(crate) abs_path: PathBuf,
    pub(crate) rel_path: String,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
}

pub enum Op<'a> { Replace(&'a str), Before(&'a str), After(&'a str), Delete }

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

pub fn replace_lines(file: String, start: usize, end: usize, body: String) -> Result<()> {
    check_range(start, end)?;
    let (abs, rel) = resolve_path(crate::db(), &file)?;
    splice_file(&abs, |lines| {
        let bl: Vec<String> = body.trim_end().lines().map(String::from).collect();
        lines.splice((start - 1)..end.min(lines.len()), bl);
        eprintln!("  Replaced L{start}-{end} in {rel}");
    })
}

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

fn splice_file(path: &Path, f: impl FnOnce(&mut Vec<String>)) -> Result<()> {
    locked_edit(path, |content| {
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        let trailing = content.ends_with('\n');
        f(&mut lines);
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        Ok(join_lines(&refs, trailing))
    })
}

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

pub(crate) fn locate_symbol(db: &Path, symbol: &str, file_hint: Option<&str>) -> Result<SymbolLocation> {
    let graph = crate::commands::intel::load_or_build_graph()?;
    let indices = graph.resolve(symbol);
    let candidates: Vec<u32> = indices.into_iter()
        .filter(|&i| file_hint.is_none_or(|f| graph.chunks[i as usize].file.ends_with(f)))
        .collect();

    if candidates.is_empty() { bail!("Symbol '{symbol}' not found"); }
    if candidates.len() > 1 {
        let locs: Vec<String> = candidates.iter()
            .map(|&i| { let c = &graph.chunks[i as usize]; format!("  {} [{}] {}:{}", c.name, c.kind,
                c.file, c.lines.map_or("?".into(), |(s, e)| format!("{s}-{e}"))) }).collect();
        bail!("Ambiguous '{symbol}' — {} matches:\n{}", candidates.len(), locs.join("\n"));
    }

    let chunk = &graph.chunks[candidates[0] as usize];
    let (start_1, end_1) = chunk.lines.context("No line range")?;

    let abs_path = resolve_abs_path(db, &chunk.file)?;
    let content = std::fs::read_to_string(&abs_path)?;
    let lines: Vec<&str> = content.lines().collect();

    let mut start = start_1.saturating_sub(1);
    let mut end = end_1.saturating_sub(1);
    if end >= lines.len() { bail!("L{start_1}-{end_1} exceeds file ({} lines)", lines.len()); }

    // Extend upward to include doc comments and attributes (excluding #[test]).
    while start > 0 {
        let p = lines[start - 1].trim();
        if p.starts_with("///") || p.starts_with("//!")
            || (p.starts_with("#[") && !p.starts_with("#[test") && !p.starts_with("#[cfg(test"))
            || p.starts_with("#![") || p.starts_with("/** ") || p.starts_with("* ") || p == "*/"
        { start -= 1; } else { break; }
    }
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
    let Ok(graph) = crate::commands::intel::load_or_build_graph() else { return };
    for &sym in symbols {
        let idxs: Vec<usize> = graph.chunks.iter().enumerate()
            .filter(|(_, c)| c.name == sym || c.name.ends_with(&format!("::{sym}")))
            .map(|(i, _)| i).collect();
        for &i in &idxs {
            let callers: Vec<_> = graph.callers[i].iter()
                .filter(|&&c| !graph.is_test[c as usize] && !idxs.contains(&(c as usize))).collect();
            if !callers.is_empty() {
                eprintln!("  warning: {sym} has {} caller(s):", callers.len());
                for &&c in &callers { eprintln!("    → {} ({})", graph.chunks[c as usize].name, graph.chunks[c as usize].file); }
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

fn insert_line_after(path: &Path, matcher: impl Fn(&str) -> bool, line: &str, skip_if: Option<&str>) -> Result<()> {
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
        if pos == 0 {
            result.insert(0, line.to_string());
            if !lines.is_empty() { result.insert(1, String::new()); }
        }
        let trailing = content.ends_with('\n');
        Ok(if trailing { result.join("\n") + "\n" } else { result.join("\n") })
    })
}

fn insert_reexport(path: &Path, reexport_line: &str) -> Result<()> {
    insert_line_after(path, |t| t.starts_with("use ") || t.starts_with("pub use "), reexport_line, None)
}

fn insert_mod_decl(path: &Path, mod_decl: &str, module_name: &str) -> Result<()> {
    let check = format!("pub mod {module_name};");
    insert_line_after(path,
        |t| (t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("pub(crate) mod ")) && t.ends_with(';'),
        mod_decl, Some(&check))
}

pub fn split(symbols: String, to: String, dry_run: bool) -> Result<()> {
    let db = crate::db();
    let symbol_names: Vec<&str> = symbols.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if symbol_names.is_empty() { bail!("--symbols must contain at least one symbol name"); }

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

    let ops: Vec<(&str, Op)> = symbol_names.iter().map(|&s| (s, Op::Delete)).collect();
    apply_edits(&ops, None)?;

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
