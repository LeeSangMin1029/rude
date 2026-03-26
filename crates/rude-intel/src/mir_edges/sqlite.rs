
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crate::data::parse::normalize_path;
use super::types::{CalleeInfo, MirChunk, MirEdgeMap};

pub fn mir_db_path(project_root: &Path) -> PathBuf {
    project_root.join("target").join("mir-edges").join("mir.db")
}

pub fn clear_mir_db(project_root: &Path, crates: &[&str]) -> Result<()> {
    let db_path = mir_db_path(project_root);
    if !db_path.exists() { return Ok(()); }
    let conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("failed to open mir.db: {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).ok();
    // Use DELETE journal mode to avoid WAL lock contention with subprocess
    conn.pragma_update(None, "journal_mode", "delete").ok();
    if crates.is_empty() {
        conn.execute_batch("DELETE FROM mir_edges; DELETE FROM mir_chunks;").ok();
    } else {
        for krate in crates {
            let cn = krate.replace('-', "_");
            conn.execute("DELETE FROM mir_edges WHERE crate_name = ?1", [&cn]).ok();
            conn.execute("DELETE FROM mir_chunks WHERE crate_name = ?1", [&cn]).ok();
        }
    }
    Ok(())
}

pub(super) fn make_crate_params(crates: &[&str]) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let placeholders = crates.iter().enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(",");
    let params = crates.iter()
        .map(|c| Box::new(c.replace('-', "_")) as Box<dyn rusqlite::types::ToSql>)
        .collect();
    (placeholders, params)
}

impl MirEdgeMap {
    pub fn from_sqlite(db_path: &Path, only_crates: Option<&[&str]>) -> Result<Self> {
        let conn = rusqlite::Connection::open(db_path)
            .with_context(|| format!("failed to open MIR sqlite: {}", db_path.display()))?;

        let mut combined = Self::default();

        let (query, params) = if let Some(crates) = only_crates {
            let (placeholders, params) = make_crate_params(crates);
            let q = format!("SELECT caller, caller_file, callee, callee_file, callee_start_line, line, is_local, crate_name FROM mir_edges WHERE crate_name IN ({})", placeholders);
            (q, params)
        } else {
            ("SELECT caller, caller_file, callee, callee_file, callee_start_line, line, is_local, crate_name FROM mir_edges".to_owned(), vec![])
        };

        let mut stmt = conn.prepare(&query).context("failed to prepare edge query")?;

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, usize>(4)?,
                row.get::<_, usize>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, String>(7)?,
            ))
        }).context("failed to query edges")?;

        for row in rows {
            let (caller, caller_file, callee, callee_file, callee_start_line, line, _is_local, crate_name) = row?;

            let file_normalized = normalize_path(&caller_file);
            combined.by_location
                .entry((file_normalized, line))
                .or_default()
                .push(callee.clone());

            let callee_file_normalized = normalize_path(&callee_file);
            // Track caller→crate mapping; a caller may appear in multiple crates
            // (e.g. lib + test), so we always keep the first association.
            combined.caller_crate.entry(caller.clone()).or_insert(crate_name.clone());
            // Also register in crate→callers reverse index immediately
            combined.crate_to_callers
                .entry(crate_name)
                .or_default()
                .push(caller.clone());
            combined.by_caller
                .entry(caller)
                .or_default()
                .push(CalleeInfo {
                    name: callee,
                    file: callee_file_normalized,
                    start_line: callee_start_line,
                    call_line: line,
                });

            combined.total += 1;
        }

        // Dedup crate_to_callers
        for callers in combined.crate_to_callers.values_mut() {
            callers.sort_unstable();
            callers.dedup();
        }

        Ok(combined)
    }

    pub fn load_chunks_from_sqlite(db_path: &Path, only_crates: Option<&[&str]>) -> Result<Vec<MirChunk>> {
        let conn = rusqlite::Connection::open(db_path)
            .with_context(|| format!("failed to open MIR sqlite: {}", db_path.display()))?;

        let (query, params) = if let Some(crates) = only_crates {
            let (placeholders, params) = make_crate_params(crates);
            let q = format!("SELECT name, file, kind, start_line, end_line, signature, visibility, is_test, body, calls, type_refs FROM mir_chunks WHERE crate_name IN ({})", placeholders);
            (q, params)
        } else {
            ("SELECT name, file, kind, start_line, end_line, signature, visibility, is_test, body, calls, type_refs FROM mir_chunks".to_owned(), vec![])
        };

        let mut stmt = conn.prepare(&query).context("failed to prepare chunk query")?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(MirChunk {
                name: row.get(0)?,
                file: row.get(1)?,
                kind: row.get(2)?,
                start_line: row.get(3)?,
                end_line: row.get(4)?,
                signature: row.get(5)?,
                visibility: row.get(6)?,
                is_test: row.get(7)?,
                body: row.get::<_, String>(8).unwrap_or_default(),
                calls: row.get::<_, String>(9).unwrap_or_default(),
                type_refs: row.get::<_, String>(10).unwrap_or_default(),
            })
        }).context("failed to query chunks")?;

        rows.collect::<std::result::Result<Vec<_>, _>>().context("failed to collect chunks")
    }
}
