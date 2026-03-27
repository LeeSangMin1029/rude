use std::path::{Path, PathBuf};
use anyhow::{Context, Result, bail};
use super::file::{splice_file, resolve_path, check_line, check_range};
use super::locate::locate_symbol;

pub enum Op<'a> { Replace(&'a str), Before(&'a str), After(&'a str), Delete }

impl Op<'_> {
    pub(crate) fn splice(&self, start: usize, end: usize, len: usize) -> (std::ops::Range<usize>, Vec<&str>) {
        match self {
            Op::Replace(b) => (start..(end + 1).min(len), b.trim_end().lines().collect()),
            Op::Before(b) => { let mut r: Vec<&str> = b.trim_end().lines().collect(); r.push(""); (start..start, r) }
            Op::After(b) => { let pos = (end + 1).min(len); let mut r = vec![""]; r.extend(b.trim_end().lines()); (pos..pos, r) }
            Op::Delete => (start..(end + 1).min(len), vec![]),
        }
    }
    pub(crate) fn label(&self, start: usize, end: usize) -> String {
        match self {
            Op::Replace(_) => format!("Replaced (L{}-{})", start + 1, end + 1),
            Op::Before(_) => format!("Inserted before (L{})", start + 1),
            Op::After(_) => format!("Inserted after (L{})", end + 1),
            Op::Delete => format!("Deleted (L{}-{})", start + 1, end + 1),
        }
    }
}

pub fn apply_edits(ops: &[(&str, Op)], file: Option<&str>) -> Result<()> {
    let db = crate::db();
    if ops.is_empty() { return Ok(()); }
    let deletes: Vec<&str> = ops.iter()
        .filter(|(_, op)| matches!(op, Op::Delete))
        .map(|(s, _)| *s).collect();
    if !deletes.is_empty() { warn_callers(&deletes); }
    let mut locs: Vec<_> = ops.iter()
        .map(|(sym, op)| locate_symbol(db, sym, file).map(|loc| (loc, *sym, op)))
        .collect::<Result<_>>()?;
    locs.sort_by(|a, b| b.0.start_line.cmp(&a.0.start_line));
    let abs_path = locs[0].0.abs_path.clone();
    let rel_path = locs[0].0.rel_path.clone();
    splice_file(&abs_path, |lines| {
        for (loc, sym, op) in &locs {
            let (drain, repl) = op.splice(loc.start_line, loc.end_line, lines.len());
            lines.splice(drain, repl.into_iter().map(String::from));
            eprintln!("  {} {sym} in {rel_path}", op.label(loc.start_line, loc.end_line));
        }
    })
}

pub fn run_batch(manifest: PathBuf) -> Result<()> {
    let content = std::fs::read_to_string(&manifest)
        .with_context(|| format!("failed to read manifest: {}", manifest.display()))?;
    let entries: Vec<BatchEntry> = serde_json::from_str(&content)
        .context("failed to parse batch manifest JSON")?;
    if entries.is_empty() { return Ok(()); }
    let db = crate::db();
    let mut resolved: Vec<(super::locate::SymbolLocation, String, String)> = Vec::with_capacity(entries.len());
    for e in &entries {
        let loc = locate_symbol(db, &e.symbol, e.file.as_deref())?;
        let body = match (&e.body, &e.body_file) {
            (Some(b), _) => b.clone(),
            (_, Some(f)) => std::fs::read_to_string(f).with_context(|| format!("read body_file: {}", f.display()))?,
            _ if e.op == "delete" => String::new(),
            _ => bail!("No body for '{}'", e.symbol),
        };
        resolved.push((loc, e.op.clone(), body));
    }
    let mut by_file: std::collections::HashMap<String, Vec<_>> = std::collections::HashMap::new();
    for (loc, op, body) in resolved {
        by_file.entry(loc.abs_path.to_string_lossy().into_owned()).or_default().push((loc, op, body));
    }
    for (_, mut ops) in by_file {
        ops.sort_by(|a, b| b.0.start_line.cmp(&a.0.start_line));
        let path = ops[0].0.abs_path.clone();
        let rel = ops[0].0.rel_path.clone();
        splice_file(&path, |lines| {
            for (loc, op_str, body) in &ops {
                let op = str_to_op(op_str, body);
                let (drain, repl) = op.splice(loc.start_line, loc.end_line, lines.len());
                lines.splice(drain, repl.into_iter().map(String::from));
                eprintln!("  {} in {rel}", op.label(loc.start_line, loc.end_line));
            }
        })?;
    }
    let db_parent = crate::db().parent().unwrap_or(Path::new(".")).to_path_buf();
    crate::commands::add::run(db_parent, &[])?;
    Ok(())
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

fn str_to_op<'a>(op: &str, body: &'a str) -> Op<'a> {
    match op {
        "replace" => Op::Replace(body),
        "insert-after" => Op::After(body),
        "insert-before" => Op::Before(body),
        _ => Op::Delete,
    }
}

#[derive(serde::Deserialize)]
struct BatchEntry {
    op: String,
    symbol: String,
    file: Option<String>,
    body: Option<String>,
    body_file: Option<PathBuf>,
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
