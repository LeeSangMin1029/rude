mod locate;
mod file_ops;
pub(crate) mod path;
mod line_cmds;
mod split;

use std::path::{Path, PathBuf};
use anyhow::{Context, Result, bail};

pub(crate) use locate::{SymbolLocation, locate_symbol};
pub use line_cmds::{insert_at, delete_lines, replace_lines, create_file};
pub use split::split;

pub enum Op<'a> { Replace(&'a str), Before(&'a str), After(&'a str), Delete }

pub fn apply_edits(ops: &[(&str, Op)], file: Option<&str>) -> Result<()> {
    let db = crate::db();
    if ops.is_empty() { return Ok(()); }
    let deletes: Vec<&str> = ops.iter()
        .filter(|(_, op)| matches!(op, Op::Delete))
        .map(|(s, _)| *s).collect();
    if !deletes.is_empty() { warn_callers(&deletes); }
    let mut locs: Vec<(SymbolLocation, &str, &Op)> = ops.iter()
        .map(|(sym, op)| {
            let loc = locate_symbol(db, sym, file).unwrap();
            (loc, *sym, op)
        }).collect();
    locs.sort_by(|a, b| b.0.start_line.cmp(&a.0.start_line));
    let abs_path = locs[0].0.abs_path.clone();
    let rel_path = locs[0].0.rel_path.clone();
    file_ops::splice_file(&abs_path, |lines| {
        for (loc, sym, op) in &locs {
            let (drain, repl) = file_ops::op_to_splice(loc.start_line, loc.end_line, op, lines.len());
            let owned: Vec<String> = repl.into_iter().map(String::from).collect();
            lines.splice(drain, owned);
            eprintln!("  {} {sym} in {rel_path}", file_ops::op_label(op, loc.start_line, loc.end_line));
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
    let mut by_file: std::collections::HashMap<String, Vec<(SymbolLocation, String, String)>> = std::collections::HashMap::new();
    for e in &entries {
        let loc = locate_symbol(db, &e.symbol, e.file.as_deref())?;
        let body = match (&e.body, &e.body_file) {
            (Some(b), _) => b.clone(),
            (_, Some(f)) => std::fs::read_to_string(f).with_context(|| format!("read body_file: {}", f.display()))?,
            _ if e.op == "delete" => String::new(),
            _ => bail!("No body for '{}'", e.symbol),
        };
        by_file.entry(loc.abs_path.to_string_lossy().into_owned()).or_default().push((loc, e.op.clone(), body));
    }
    for (_, mut ops) in by_file {
        ops.sort_by(|a, b| b.0.start_line.cmp(&a.0.start_line));
        let path = ops[0].0.abs_path.clone();
        let rel = ops[0].0.rel_path.clone();
        file_ops::splice_file(&path, |lines| {
            for (loc, op, body) in &ops {
                let (drain, repl) = match op.as_str() {
                    "replace" => (loc.start_line..(loc.end_line + 1).min(lines.len()), body.trim_end().lines().collect()),
                    "delete" => (loc.start_line..(loc.end_line + 1).min(lines.len()), vec![]),
                    "insert-after" => { let pos = (loc.end_line + 1).min(lines.len()); let mut r = vec![""]; r.extend(body.trim_end().lines()); (pos..pos, r) }
                    "insert-before" => { let mut r: Vec<&str> = body.trim_end().lines().collect(); r.push(""); (loc.start_line..loc.start_line, r) }
                    _ => continue,
                };
                lines.splice(drain, repl.into_iter().map(String::from));
                eprintln!("  {op} {} in {rel}", loc.start_line + 1);
            }
        })?;
    }
    let db_parent = crate::db().parent().unwrap_or(Path::new(".")).to_path_buf();
    crate::commands::add::run(db_parent, &[])?;
    Ok(())
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
