//! Symbolic code editing — replace, insert, delete by symbol name.
//!
//! Uses the rude index DB to locate symbols, then edits the source file directly.
//! Doc comments and attributes above the symbol are automatically included in the range.
//! Also provides line-based editing: `insert_at`, `delete_lines`, `replace_lines`, and `create_file`.
//!
//! ## Concurrency
//!
//! All edit operations acquire an exclusive file lock (`fs2::lock_exclusive`) on the
//! target source file for the entire read-modify-write cycle. This prevents data loss
//! when multiple agents edit the same file concurrently. The lock is released
//! automatically when the guard is dropped.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

use rude_intel::loader::load_chunks;
use rude_intel::helpers::find_project_root;
use rude_intel::parse::ParsedChunk;

// ── Symbol location ─────────────────────────────────────────────────────

/// Resolved symbol location in the current source file.
pub(crate) struct SymbolLocation {
    /// Absolute path to the source file.
    pub(crate) abs_path: PathBuf,
    /// Relative path (from DB) for display.
    pub(crate) rel_path: String,
    /// 0-based start line (inclusive).
    pub(crate) start_line: usize,
    /// 0-based end line (inclusive).
    pub(crate) end_line: usize,
}

/// Find a symbol in the DB and resolve its current location in the source file.
///
/// `symbol` is matched against chunk names (exact or `::suffix` match).
/// `file_hint` narrows the search to chunks whose file path ends with the given suffix.
///
/// Uses a HashMap index for O(1) lookup instead of linear scan over all chunks.
pub(crate) fn locate_symbol(db: &Path, symbol: &str, file_hint: Option<&str>) -> Result<SymbolLocation> {
    let chunks = load_chunks(db)?;

    // Build name → indices map for fast lookup.
    let mut name_index: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, c) in chunks.iter().enumerate() {
        name_index.entry(&c.name).or_default().push(i);
        // Index by last segment only if different from full name
        if let Some(suffix) = c.name.rsplit("::").next() {
            if suffix != c.name {
                name_index.entry(suffix).or_default().push(i);
            }
        }
    }

    // O(1) lookup by exact name or last segment
    let mut candidate_indices = name_index.get(symbol).cloned().unwrap_or_default();

    // Fallback: linear scan for intermediate paths (e.g. "MirEdgeMap::from_dir")
    if candidate_indices.is_empty() && symbol.contains("::") {
        for (i, c) in chunks.iter().enumerate() {
            if c.name.ends_with(&format!("::{symbol}")) || c.name == symbol {
                candidate_indices.push(i);
            }
        }
    }

    let candidates: Vec<&ParsedChunk> = candidate_indices
        .into_iter()
        .map(|i| &chunks[i])
        .filter(|c| {
            let name_match = c.name == symbol || c.name.ends_with(&format!("::{symbol}"));
            let file_match = file_hint.is_none_or(|f| c.file.ends_with(f));
            name_match && file_match
        })
        .collect();

    if candidates.is_empty() {
        bail!(
            "Symbol '{symbol}' not found{}",
            file_hint.map_or(String::new(), |f| format!(" in file matching '{f}'"))
        );
    }

    if candidates.len() > 1 {
        let locations: Vec<String> = candidates
            .iter()
            .map(|c| {
                let lines = c
                    .lines
                    .map_or("?".to_owned(), |(s, e)| format!("{s}-{e}"));
                format!("  {} [{}] {}:{lines}", c.name, c.kind, c.file)
            })
            .collect();
        bail!(
            "Ambiguous symbol '{symbol}' — {} matches found. Use --file to narrow:\n{}",
            candidates.len(),
            locations.join("\n")
        );
    }

    let chunk = candidates[0];
    let (start_1, end_1) = chunk
        .lines
        .context("Symbol has no line range in DB (re-index with `rude add`)")?;

    // Resolve absolute path: prefer CWD over DB parent for worktree support.
    // In worktrees, DB may point to main repo but CWD is the worktree.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let db_parent = db.parent().unwrap_or_else(|| Path::new("."));
    let db_parent = if db_parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        db_parent
    };

    // Try CWD first (worktree), then DB parent (main repo)
    let chunk_path = PathBuf::from(&chunk.file);
    let abs_path = if chunk_path.is_absolute() {
        rude_db::strip_unc_prefix_path(&chunk_path)
    } else {
        let cwd_path = cwd.join(&chunk.file);
        if cwd_path.exists() {
            cwd_path
        } else {
            let db_root = db_parent
                .canonicalize()
                .unwrap_or_else(|_| db_parent.to_path_buf());
            let db_root = rude_db::strip_unc_prefix_path(&db_root);
            db_root.join(&chunk.file)
        }
    };
    let project_root = if cwd.join(&chunk.file).exists() {
        cwd.clone()
    } else {
        db_parent.canonicalize().unwrap_or_else(|_| db_parent.to_path_buf())
    };
    let project_root = rude_db::strip_unc_prefix_path(&project_root);
    if !abs_path.exists() {
        bail!(
            "Source file not found: {} (resolved to {})",
            chunk.file,
            abs_path.display()
        );
    }
    // Compute relative path for display.
    let norm_root = project_root.to_string_lossy().replace('\\', "/");
    let norm_file = chunk.file.replace('\\', "/");
    let norm_file = rude_db::strip_unc_prefix(&norm_file);
    let rel_display = norm_file
        .strip_prefix(norm_root.as_str())
        .and_then(|s| s.strip_prefix('/'))
        .unwrap_or(norm_file)
        .to_owned();

    // Convert 1-based (DB) to 0-based.
    let mut start_line = start_1.saturating_sub(1);
    let end_line = end_1.saturating_sub(1);

    // Sanity check: verify the file has enough lines.
    let content = std::fs::read_to_string(&abs_path)
        .with_context(|| format!("Failed to read {}", abs_path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    if end_line >= lines.len() {
        bail!(
            "DB range L{start_1}-{end_1} exceeds file length ({} lines). \
             File may have changed — run `rude add` to re-index.",
            lines.len()
        );
    }

    // Extend start upward to include leading doc comments and attributes.
    // This ensures `replace` captures the full definition including docs.
    while start_line > 0 {
        let prev = lines[start_line - 1].trim();
        if prev.starts_with("///")
            || prev.starts_with("//!")
            || (prev.starts_with("#[") && !prev.starts_with("#[test") && !prev.starts_with("#[cfg(test"))
            || prev.starts_with("#![")
            || prev.starts_with("/** ")
            || prev.starts_with("* ")
            || prev == "*/"
            // Python/JS/TS decorators and docstrings
            || prev.starts_with('@')
            || prev.starts_with("\"\"\"")
            || prev.starts_with("'''")
            // Go doc comments
            || prev.starts_with("//")
                && start_line >= 2
                && lines.get(start_line.saturating_sub(2))
                    .is_some_and(|l| l.trim().starts_with("//"))
        {
            start_line -= 1;
        } else {
            break;
        }
    }

    // Extend end downward to include closing brace if the symbol's end line
    // doesn't already contain one (e.g. MIR span covers only the signature).
    let mut end_line = end_line;
    let has_open_brace = (start_line..=end_line).any(|i| lines[i].contains('{'));
    if has_open_brace {
        let has_close_brace = (start_line..=end_line).any(|i| lines[i].contains('}'));
        if !has_close_brace {
            // Find the matching closing brace by counting braces
            let mut depth: i32 = 0;
            for i in start_line..lines.len() {
                for ch in lines[i].chars() {
                    if ch == '{' { depth += 1; }
                    if ch == '}' { depth -= 1; }
                }
                if i > end_line && depth <= 0 {
                    end_line = i;
                    break;
                }
            }
        }
    }

    Ok(SymbolLocation {
        abs_path,
        rel_path: rel_display,
        start_line,
        end_line,
    })
}

// ── Edit operations ─────────────────────────────────────────────────────

/// Locate a symbol and load the source file in one step.
///
/// Returns lines and metadata needed by all symbol-editing commands.

/// Write lines back to a file, preserving the original trailing-newline style.
pub(crate) fn join_lines(lines: &[&str], trailing_nl: bool) -> String {
    if trailing_nl {
        lines.join("\n") + "\n"
    } else {
        lines.join("\n")
    }
}

/// Perform an exclusive-locked read-modify-write on a source file.
///
/// Uses a `.lock` sidecar file for mutual exclusion (works on Windows where
/// `lock_exclusive` on a file blocks other handles from opening it).
/// The lock file is created next to the target and cleaned up afterward.
pub(crate) fn locked_edit<F>(path: &Path, transform: F) -> Result<()>
where
    F: FnOnce(&str) -> Result<String>,
{
    let lock_path = path.with_extension("lock");
    let lock_file = File::create(&lock_path)
        .with_context(|| format!("Failed to create lock file {}", lock_path.display()))?;
    lock_file.lock_exclusive()
        .with_context(|| format!("Failed to acquire file lock on {}", lock_path.display()))?;

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let output = transform(&content)?;
    std::fs::write(path, output)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    lock_file.unlock()
        .with_context(|| format!("Failed to release file lock on {}", lock_path.display()))?;
    drop(lock_file);
    let _ = std::fs::remove_file(&lock_path);
    Ok(())
}

/// Replace the body of a symbol with new content.
/// Edit operation on a symbol range.
enum Op<'a> {
    /// Replace symbol body with new content.
    Replace(&'a str),
    /// Insert content before the symbol.
    Before(&'a str),
    /// Insert content after the symbol.
    After(&'a str),
    /// Delete the symbol (= replace with nothing).
    Delete,
}

/// Core: locate symbols → apply ops in one locked write → no line drift.
/// All edit commands (replace, insert, delete) funnel through here.
fn apply_edits(db: &Path, ops: &[(&str, Op)], file: Option<&str>) -> Result<()> {
    if ops.is_empty() { return Ok(()); }

    // Warn callers for any Delete ops
    let deletes: Vec<&str> = ops.iter()
        .filter(|(_, op)| matches!(op, Op::Delete))
        .map(|(s, _)| *s)
        .collect();
    if !deletes.is_empty() {
        warn_callers(db, &deletes);
    }

    // Locate all symbols
    let mut edits: Vec<(usize, usize, &str, &Op)> = Vec::new();
    for (sym, op) in ops {
        let loc = locate_symbol(db, sym, file)?;
        edits.push((loc.start_line, loc.end_line, sym, op));
    }
    // Sort back-to-front so line numbers stay valid
    edits.sort_by(|a, b| b.0.cmp(&a.0));

    let first = locate_symbol(db, ops[0].0, file)?;
    locked_edit(&first.abs_path, |content| {
        let mut lines: Vec<&str> = content.lines().collect();

        for &(start, end, sym, op) in &edits {
            match op {
                Op::Replace(body) => {
                    let body = body.trim_end();
                    let body_lines: Vec<&str> = body.lines().collect();
                    let remove_end = (end + 1).min(lines.len());
                    lines.splice(start..remove_end, body_lines.iter().copied());
                    eprintln!("Replaced {sym} (L{}-{}) in {}", start + 1, end + 1, first.rel_path);
                }
                Op::Before(body) => {
                    let body = body.trim_end();
                    let mut insert: Vec<&str> = body.lines().collect();
                    insert.push("");
                    for (i, line) in insert.iter().enumerate() {
                        lines.insert(start + i, line);
                    }
                    eprintln!("Inserted before {sym} (before L{}) in {}", start + 1, first.rel_path);
                }
                Op::After(body) => {
                    let body = body.trim_end();
                    let mut insert: Vec<&str> = vec![""];
                    insert.extend(body.lines());
                    let pos = (end + 1).min(lines.len());
                    for (i, line) in insert.iter().enumerate() {
                        lines.insert(pos + i, line);
                    }
                    eprintln!("Inserted after {sym} (after L{}) in {}", end + 1, first.rel_path);
                }
                Op::Delete => {
                    let remove_end = (end + 1).min(lines.len());
                    let mut after = remove_end;
                    while after < lines.len() && lines[after].trim().is_empty() { after += 1; }
                    lines.drain(start..after);
                    eprintln!("Deleted {sym} (L{}-{}) from {}", start + 1, end + 1, first.rel_path);
                }
            }
        }
        Ok(join_lines(&lines, content.ends_with('\n')))
    })
}

fn warn_callers(db: &Path, symbols: &[&str]) {
    let Ok((graph, _)) = crate::commands::intel::load_or_build_graph_with_chunks(db) else { return };
    for &sym in symbols {
        let idxs: Vec<usize> = graph.names.iter().enumerate()
            .filter(|(_, n)| *n == sym || n.ends_with(&format!("::{sym}")))
            .map(|(i, _)| i).collect();
        for &idx in &idxs {
            let callers: Vec<_> = graph.callers[idx].iter()
                .filter(|&&c| !graph.is_test[c as usize] && !idxs.contains(&(c as usize)))
                .collect();
            if !callers.is_empty() {
                eprintln!("  warning: {sym} has {} caller(s):", callers.len());
                for &&c in &callers {
                    eprintln!("    → {} ({})", graph.names[c as usize], graph.files[c as usize]);
                }
            }
        }
    }
}

// ── Public API: thin wrappers around apply_edits ────────────────────

pub fn replace(db: PathBuf, symbol: String, file: Option<String>, body: String) -> Result<()> {
    apply_edits(&db, &[(&symbol, Op::Replace(&body))], file.as_deref())
}

pub fn insert_after(db: PathBuf, symbol: String, file: Option<String>, body: String) -> Result<()> {
    apply_edits(&db, &[(&symbol, Op::After(&body))], file.as_deref())
}

pub fn insert_before(db: PathBuf, symbol: String, file: Option<String>, body: String) -> Result<()> {
    apply_edits(&db, &[(&symbol, Op::Before(&body))], file.as_deref())
}

pub fn delete_symbol(db: PathBuf, symbol: String, file: Option<String>) -> Result<()> {
    apply_edits(&db, &[(&symbol, Op::Delete)], file.as_deref())
}

pub fn delete_symbols(db: PathBuf, symbols: &[&str], file: Option<&str>) -> Result<()> {
    let ops: Vec<(&str, Op)> = symbols.iter().map(|&s| (s, Op::Delete)).collect();
    apply_edits(&db, &ops, file)
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Print lines with 1-based line numbers to stdout, so agents can verify edits.
fn print_numbered_range(lines: &[&str], start_1based: usize) {
    for (i, line) in lines.iter().enumerate() {
        println!("{:>4}\u{2502} {line}", start_1based + i);
    }
}

// ── Line-based editing ──────────────────────────────────────────────────

/// Resolve a project-relative file path — prefer CWD (worktree) over DB root.
fn resolve_file_path(db: &Path, file: &str) -> Result<(PathBuf, String)> {
    // Try CWD first (supports worktrees)
    let cwd = std::env::current_dir().unwrap_or_default();
    let cwd_path = cwd.join(file);
    if cwd_path.exists() {
        return Ok((cwd_path, file.to_string()));
    }
    // Fall back to DB-relative
    let root = find_project_root(db)
        .context("Cannot determine project root from DB path")?;
    let abs_path = root.join(file);
    if !abs_path.exists() {
        bail!("File not found: {} (resolved to {})", file, abs_path.display());
    }
    Ok((abs_path, file.to_string()))
}

/// Insert content at a specific 1-based line number (before that line).
pub fn insert_at(db: PathBuf, file: String, line: usize, body: String) -> Result<()> {
    if line == 0 {
        bail!("--line must be >= 1 (1-based)");
    }

    let (abs_path, rel_path) = resolve_file_path(&db, &file)?;
    let body = body.trim_end().to_owned();

    locked_edit(&abs_path, |content| {
        let lines: Vec<&str> = content.lines().collect();
        let idx = line - 1;
        if idx > lines.len() {
            bail!(
                "--line {} is past end of file ({} lines)",
                line,
                lines.len()
            );
        }

        let body_lines: Vec<&str> = body.lines().collect();

        let mut result: Vec<&str> = Vec::with_capacity(lines.len() + body_lines.len());
        result.extend_from_slice(&lines[..idx]);
        result.extend_from_slice(&body_lines);
        if idx < lines.len() {
            result.extend_from_slice(&lines[idx..]);
        }

        eprintln!(
            "Inserted {} line(s) at L{} in {}",
            body_lines.len(),
            line,
            rel_path,
        );
        print_numbered_range(&body_lines, line);
        Ok(join_lines(&result, content.ends_with('\n')))
    })
}

/// Delete a range of lines (1-based, inclusive) from a file.
pub fn delete_lines(db: PathBuf, file: String, start: usize, end: usize) -> Result<()> {
    if start == 0 || end == 0 {
        bail!("--start and --end must be >= 1 (1-based)");
    }
    if start > end {
        bail!("--start ({start}) must be <= --end ({end})");
    }

    let (abs_path, rel_path) = resolve_file_path(&db, &file)?;

    locked_edit(&abs_path, |content| {
        let lines: Vec<&str> = content.lines().collect();

        if end > lines.len() {
            bail!(
                "--end {} is past end of file ({} lines)",
                end,
                lines.len()
            );
        }

        let start_idx = start - 1;
        let end_idx = end;

        let mut result: Vec<&str> = Vec::with_capacity(lines.len());
        result.extend_from_slice(&lines[..start_idx]);

        let mut after = end_idx;
        while after < lines.len() && lines[after].trim().is_empty() {
            after += 1;
        }
        if after < lines.len() {
            result.extend_from_slice(&lines[after..]);
        }

        eprintln!(
            "Deleted L{}-{} ({} line(s)) from {}",
            start,
            end,
            end - start + 1,
            rel_path,
        );
        Ok(join_lines(&result, content.ends_with('\n')))
    })
}

/// Replace a range of lines (1-based, inclusive) with new content.
pub fn replace_lines(
    db: PathBuf,
    file: String,
    start: usize,
    end: usize,
    body: String,
) -> Result<()> {
    if start == 0 || end == 0 {
        bail!("--start and --end must be >= 1 (1-based)");
    }
    if start > end {
        bail!("--start ({start}) must be <= --end ({end})");
    }

    let (abs_path, rel_path) = resolve_file_path(&db, &file)?;
    let body = body.trim_end().to_owned();

    locked_edit(&abs_path, |content| {
        let lines: Vec<&str> = content.lines().collect();

        if end > lines.len() {
            bail!(
                "--end {} is past end of file ({} lines)",
                end,
                lines.len()
            );
        }

        let start_idx = start - 1;
        let end_idx = end;
        let body_lines: Vec<&str> = body.lines().collect();

        let mut result: Vec<&str> = Vec::with_capacity(lines.len() + body_lines.len());
        result.extend_from_slice(&lines[..start_idx]);
        result.extend_from_slice(&body_lines);
        if end_idx < lines.len() {
            result.extend_from_slice(&lines[end_idx..]);
        }

        let new_end = start + body_lines.len().saturating_sub(1);
        eprintln!(
            "Replaced L{}-{} -> L{}-{} ({} line(s)) in {}",
            start,
            end,
            start,
            new_end,
            body_lines.len(),
            rel_path,
        );
        print_numbered_range(&body_lines, start);
        Ok(join_lines(&result, content.ends_with('\n')))
    })
}

/// Create a new file at a project-relative path.
///
/// Fails if the file already exists. Parent directories are created automatically.
pub fn create_file(db: PathBuf, file: String, body: String) -> Result<()> {
    let root = find_project_root(&db)
        .context("Cannot determine project root from DB path")?;
    let abs_path = root.join(&file);

    if abs_path.exists() {
        bail!(
            "File already exists: {} (use replace-lines to edit)",
            abs_path.display()
        );
    }

    if let Some(parent) = abs_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directories for {}", parent.display()))?;
    }

    let body = body.trim_end();
    let content = if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    };
    std::fs::write(&abs_path, &content)
        .with_context(|| format!("Failed to write file: {}", abs_path.display()))?;

    let body_lines: Vec<&str> = body.lines().collect();
    eprintln!("Created {} ({} line(s))", file, body_lines.len());
    print_numbered_range(&body_lines, 1);
    Ok(())
}
