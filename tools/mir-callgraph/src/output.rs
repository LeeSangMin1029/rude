use std::io::Write;

use crate::types::{CallEdge, MirChunk, UseItem, UseDep};

pub fn write_results(
    crate_name: &str,
    edges: &[CallEdge],
    chunks: &[MirChunk],
    uses: &[UseItem],
    use_deps: &[UseDep],
    fn_count: usize,
    json: bool,
    db_path: &Option<String>,
) {
    if let Some(db) = db_path {
        write_sqlite(db, crate_name, edges, chunks, uses, use_deps);
    } else if json {
        write_stdout(edges);
    }

    // Always log to stderr
    if db_path.is_some() {
        eprintln!(
            "[mir-callgraph] {crate_name}: {} edges, {} chunks ({fn_count} fns)",
            edges.len(), chunks.len()
        );
    }
}

fn write_sqlite(db_path: &str, crate_name: &str, edges: &[CallEdge], chunks: &[MirChunk], uses: &[UseItem], use_deps: &[UseDep]) {
    if let Some(parent) = std::path::Path::new(db_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = match rusqlite::Connection::open(db_path) {
        Ok(c) => c,
        Err(e) => { eprintln!("[mir-callgraph] failed to open DB {db_path}: {e}"); return; }
    };
    let _ = conn.pragma_update(None, "journal_mode", "wal");
    let _ = conn.pragma_update(None, "synchronous", "off");
    conn.busy_timeout(std::time::Duration::from_secs(30)).ok();

    let _ = conn.execute_batch("
        CREATE TABLE IF NOT EXISTS mir_edges (
            caller TEXT, caller_file TEXT, caller_kind TEXT,
            callee TEXT, callee_file TEXT, callee_start_line INTEGER,
            line INTEGER, is_local INTEGER, crate_name TEXT,
            UNIQUE(caller, callee, line, crate_name)
        );
        CREATE TABLE IF NOT EXISTS mir_chunks (
            name TEXT, file TEXT, kind TEXT,
            start_line INTEGER, end_line INTEGER,
            signature TEXT, visibility TEXT, is_test INTEGER,
            body TEXT, calls TEXT, type_refs TEXT, crate_name TEXT,
            UNIQUE(name, kind, crate_name)
        );
        CREATE TABLE IF NOT EXISTS mir_uses (
            file TEXT, line INTEGER, source TEXT, resolved TEXT, crate_name TEXT,
            UNIQUE(file, line, crate_name)
        );
        CREATE TABLE IF NOT EXISTS mir_use_deps (
            fn_name TEXT, fn_file TEXT, use_file TEXT, use_line INTEGER, crate_name TEXT
        );
    ");

    let Ok(tx) = conn.unchecked_transaction() else { return };
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO mir_edges VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"
        ).unwrap();
        for e in edges {
            let _ = stmt.execute(rusqlite::params![
                e.caller, e.caller_file, e.caller_kind,
                e.callee, e.callee_file, e.callee_start_line,
                e.line, e.is_local as i32, crate_name,
            ]);
        }
    }
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO mir_chunks VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"
        ).unwrap();
        for c in chunks {
            let _ = stmt.execute(rusqlite::params![
                c.name, c.file, c.kind,
                c.start_line, c.end_line,
                c.signature, c.visibility, c.is_test as i32,
                "", c.calls, c.type_refs, crate_name,
            ]);
        }
    }
    {
        let _ = tx.execute("DELETE FROM mir_uses WHERE crate_name = ?1", rusqlite::params![crate_name]);
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO mir_uses VALUES (?1,?2,?3,?4,?5)"
        ).unwrap();
        for u in uses {
            let _ = stmt.execute(rusqlite::params![u.file, u.line, u.source, u.resolved, crate_name]);
        }
    }
    {
        let _ = tx.execute("DELETE FROM mir_use_deps WHERE crate_name = ?1", rusqlite::params![crate_name]);
        let mut stmt = tx.prepare_cached(
            "INSERT INTO mir_use_deps VALUES (?1,?2,?3,?4,?5)"
        ).unwrap();
        for d in use_deps {
            let _ = stmt.execute(rusqlite::params![d.fn_name, d.fn_file, d.use_file, d.use_line, crate_name]);
        }
    }
    let _ = tx.commit();
}

fn write_stdout(edges: &[CallEdge]) {
    let mut w = std::io::BufWriter::new(std::io::stdout().lock());
    for e in edges { if let Ok(s) = serde_json::to_string(e) { let _ = writeln!(w, "{s}"); } }
}
