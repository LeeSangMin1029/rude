
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crate::data::parse::normalize_path;
use super::types::{CalleeInfo, MirChunk, MirEdgeMap};

pub fn mir_db_path(project_root: &Path) -> PathBuf {
    project_root.join("target").join("mir-edges").join("mir.db")
}

pub fn mir_crate_names(project_root: &Path) -> Vec<String> {
    let mir_db = mir_db_path(project_root);
    if !mir_db.exists() { return Vec::new(); }
    rusqlite::Connection::open(&mir_db).ok()
        .and_then(|conn| {
            let mut stmt = conn.prepare("SELECT DISTINCT crate_name FROM mir_chunks").ok()?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0)).ok()?;
            Some(rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
}

pub fn merge_mir_db(main_db: &Path, sub_db: &Path, main_root: &Path, sub_root: &Path) -> Result<()> {
    let conn = rusqlite::Connection::open(main_db)?;
    let sub_path = sub_db.display().to_string().replace('\\', "/");
    let local_crates = super::workspace::detect_workspace_crate_names(sub_root);
    let file_prefix = sub_root.strip_prefix(main_root).ok()
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    conn.execute_batch(&format!("ATTACH DATABASE '{sub_path}' AS sub;"))?;
    let placeholders = if local_crates.is_empty() { String::new() }
        else { local_crates.iter().map(|c| format!("'{}'", c.replace('\'', "''"))).collect::<Vec<_>>().join(",") };
    let crate_filter = if placeholders.is_empty() { String::new() }
        else { format!(" WHERE crate_name IN ({placeholders})") };
    conn.execute_batch(&format!(
        "INSERT OR REPLACE INTO mir_edges SELECT * FROM sub.mir_edges{crate_filter};"
    ))?;
    if file_prefix.is_empty() {
        conn.execute_batch(&format!(
            "INSERT OR REPLACE INTO mir_chunks SELECT * FROM sub.mir_chunks{crate_filter};"
        ))?;
    } else {
        // rewrite file paths: prepend sub-workspace relative path
        let mut stmt = conn.prepare(&format!(
            "SELECT name, file, kind, start_line, end_line, signature, visibility, is_test, body, calls, type_refs, crate_name FROM sub.mir_chunks{crate_filter}"
        ))?;
        let mut insert = conn.prepare(
            "INSERT OR REPLACE INTO mir_chunks (name, file, kind, start_line, end_line, signature, visibility, is_test, body, calls, type_refs, crate_name) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
        )?;
        let rows = stmt.query_map([], |row| {
            let file: String = row.get(1)?;
            let is_absolute = file.starts_with('/') || (file.len() > 2 && file.as_bytes()[1] == b':');
            let already_prefixed = file.starts_with(&file_prefix);
            let prefixed = if is_absolute || already_prefixed { file } else { format!("{file_prefix}/{file}") };
            Ok((row.get::<_, String>(0)?, prefixed, row.get::<_, String>(2)?,
                row.get::<_, u32>(3)?, row.get::<_, u32>(4)?,
                row.get::<_, Option<String>>(5)?, row.get::<_, Option<String>>(6)?,
                row.get::<_, bool>(7)?, row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?, row.get::<_, Option<String>>(10)?,
                row.get::<_, String>(11)?))
        })?;
        for row in rows {
            let r = row?;
            insert.execute(rusqlite::params![r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9, r.10, r.11])?;
        }
    }
    conn.execute_batch("DETACH DATABASE sub;")?;
    Ok(())
}

pub fn clear_mir_db(project_root: &Path, crates: &[&str]) -> Result<()> {
    let db_path = mir_db_path(project_root);
    if !db_path.exists() { return Ok(()); }
    let conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("failed to open mir.db: {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).ok();
    conn.pragma_update(None, "journal_mode", "delete").ok();
    conn.pragma_update(None, "synchronous", "off").ok();
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
                .entry((file_normalized.clone(), line))
                .or_default()
                .push(callee.clone());
            if !file_normalized.is_empty() {
                let files = combined.caller_files.entry(caller.clone()).or_default();
                if !files.contains(&file_normalized) { files.push(file_normalized); }
            }

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
            let q = format!("SELECT name, file, kind, start_line, end_line, signature, visibility, is_test, body, calls, type_refs, crate_name FROM mir_chunks WHERE crate_name IN ({})", placeholders);
            (q, params)
        } else {
            ("SELECT name, file, kind, start_line, end_line, signature, visibility, is_test, body, calls, type_refs, crate_name FROM mir_chunks".to_owned(), vec![])
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
                crate_name: row.get::<_, String>(11).unwrap_or_default(),
            })
        }).context("failed to query chunks")?;

        rows.collect::<std::result::Result<Vec<_>, _>>().context("failed to collect chunks")
    }
}
