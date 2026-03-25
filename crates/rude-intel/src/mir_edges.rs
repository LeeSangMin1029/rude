//! MIR-based call edge extraction via `mir-callgraph` subprocess.
//!
//! Parses JSONL output from the `mir-callgraph` tool and provides
//! resolved call edges for accurate graph construction.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use crate::parse::normalize_path;

/// Collection of MIR edges indexed for fast lookup by caller.
#[derive(Debug, Default)]
pub struct MirEdgeMap {
    /// (caller_file_normalized, line) → Vec<callee_name>
    pub by_location: HashMap<(String, usize), Vec<String>>,
    /// caller_name → Vec<(callee_name, callee_file, callee_start_line, call_line)>
    pub by_caller: HashMap<String, Vec<CalleeInfo>>,
    /// Total edge count
    pub total: usize,
    /// caller_name → crate_name (tracks which crate each caller belongs to)
    pub caller_crate: HashMap<String, String>,
    /// Reverse index: crate_name → Vec<caller_name> (for O(1) crate lookup)
    pub crate_to_callers: HashMap<String, Vec<String>>,
}

/// Callee information from a MIR edge.
#[derive(Debug, Clone)]
pub struct CalleeInfo {
    pub name: String,
    pub file: String,
    pub start_line: usize,
    pub call_line: usize,
}

impl MirEdgeMap {
    /// Load MIR edges from a JSONL file.
    /// Load all `.edges.jsonl` files from a directory.
    /// Load all edge JSONL files from a directory.
    /// Load edge JSONL files, optionally filtering to specific crates only.
    /// When `only_crates` is Some, only loads JSONL for those crate names.
    /// Load edges from sqlite (mir-callgraph direct write mode).
    pub fn from_sqlite(db_path: &Path, only_crates: Option<&[&str]>) -> Result<Self> {
        let conn = rusqlite::Connection::open(db_path)
            .with_context(|| format!("failed to open MIR sqlite: {}", db_path.display()))?;

        let mut combined = Self::default();

        let query = if let Some(crates) = only_crates {
            let placeholders: Vec<String> = crates.iter().enumerate()
                .map(|(i, _)| format!("?{}", i + 1))
                .collect();
            format!("SELECT caller, caller_file, callee, callee_file, callee_start_line, line, is_local, crate_name FROM mir_edges WHERE crate_name IN ({})", placeholders.join(","))
        } else {
            "SELECT caller, caller_file, callee, callee_file, callee_start_line, line, is_local, crate_name FROM mir_edges".to_owned()
        };

        let mut stmt = conn.prepare(&query).context("failed to prepare edge query")?;

        let params: Vec<Box<dyn rusqlite::types::ToSql>> = if let Some(crates) = only_crates {
            crates.iter().map(|c| {
                let normalized = c.replace('-', "_");
                Box::new(normalized) as Box<dyn rusqlite::types::ToSql>
            }).collect()
        } else {
            vec![]
        };
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

    /// Load MIR chunks from sqlite.
    pub fn load_chunks_from_sqlite(db_path: &Path, only_crates: Option<&[&str]>) -> Result<Vec<MirChunk>> {
        let conn = rusqlite::Connection::open(db_path)
            .with_context(|| format!("failed to open MIR sqlite: {}", db_path.display()))?;

        let query = if let Some(crates) = only_crates {
            let placeholders: Vec<String> = crates.iter().enumerate()
                .map(|(i, _)| format!("?{}", i + 1))
                .collect();
            format!("SELECT name, file, kind, start_line, end_line, signature, visibility, is_test, body, calls, type_refs FROM mir_chunks WHERE crate_name IN ({})", placeholders.join(","))
        } else {
            "SELECT name, file, kind, start_line, end_line, signature, visibility, is_test, body, calls, type_refs FROM mir_chunks".to_owned()
        };

        let mut stmt = conn.prepare(&query).context("failed to prepare chunk query")?;
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = if let Some(crates) = only_crates {
            crates.iter().map(|c| Box::new(c.replace('-', "_")) as Box<dyn rusqlite::types::ToSql>).collect()
        } else {
            vec![]
        };
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

    /// Get the set of all unique crate names present in this edge map.
    pub fn crate_names(&self) -> std::collections::HashSet<&str> {
        self.caller_crate.values().map(String::as_str).collect()
    }

    /// Get callers belonging to a specific crate.
    /// Uses a pre-built reverse index for O(1) lookup instead of O(N) scan.
    pub fn callers_for_crate<'a>(&'a self, crate_name: &str) -> Vec<&'a str> {
        self.crate_to_callers.get(crate_name)
            .map(|callers| callers.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

/// Get the nightly sysroot bin path for rustc_driver DLL resolution.
/// mir-callgraph dynamically links rustc_driver, which lives in the nightly bin dir.
fn nightly_sysroot_bin() -> Option<String> {
    Command::new("rustc")
        .args(["+nightly", "--print", "sysroot"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| {
            let sysroot = s.trim();
            format!("{sysroot}/bin")
        })
}

/// Append nightly sysroot/bin to a Command's PATH so rustc_driver DLL is found.
fn add_nightly_path(cmd: &mut Command) {
    if let Some(nightly_bin) = nightly_sysroot_bin() {
        let current = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{current};{nightly_bin}"));
    }
}

// Embedded mir-callgraph source files for auto-build.
const MIR_CALLGRAPH_MAIN_RS: &str =
    include_str!("../../../tools/mir-callgraph/src/main.rs");
const MIR_CALLGRAPH_CARGO_TOML: &str =
    include_str!("../../../tools/mir-callgraph/Cargo.toml");
const MIR_CALLGRAPH_RUST_TOOLCHAIN: &str =
    include_str!("../../../tools/mir-callgraph/rust-toolchain.toml");

/// Binary name for mir-callgraph (platform-dependent).
fn mir_callgraph_bin_name() -> &'static str {
    if cfg!(windows) { "mir-callgraph.exe" } else { "mir-callgraph" }
}

/// Base directory for rude data: `~/.rude/`.
fn rude_home() -> Result<PathBuf> {
    let home = rude_db::home_dir()
        .context("cannot determine home directory")?;
    Ok(home.join(".rude"))
}

/// Get the current nightly rustc version string, or None if nightly is not installed.
fn nightly_rustc_version() -> Option<String> {
    Command::new("rustc")
        .args(["+nightly", "--version"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
}

/// Extract embedded mir-callgraph source to the build directory.
fn extract_mir_callgraph_source(build_dir: &Path) -> Result<()> {
    let src_dir = build_dir.join("src");
    std::fs::create_dir_all(&src_dir)
        .with_context(|| format!("failed to create build dir: {}", src_dir.display()))?;

    std::fs::write(build_dir.join("Cargo.toml"), MIR_CALLGRAPH_CARGO_TOML)
        .context("failed to write Cargo.toml")?;
    std::fs::write(build_dir.join("rust-toolchain.toml"), MIR_CALLGRAPH_RUST_TOOLCHAIN)
        .context("failed to write rust-toolchain.toml")?;
    std::fs::write(src_dir.join("main.rs"), MIR_CALLGRAPH_MAIN_RS)
        .context("failed to write main.rs")?;

    Ok(())
}

/// Build mir-callgraph from embedded source using nightly toolchain.
fn build_mir_callgraph(base: &Path) -> Result<PathBuf> {
    let nightly_ver = nightly_rustc_version().ok_or_else(|| {
        anyhow::anyhow!(
            "nightly Rust toolchain required for rude add.\n\
             Run: rustup toolchain install nightly --component rust-src rustc-dev llvm-tools-preview"
        )
    })?;

    let bin_dir = base.join("bin");
    let cached_bin = bin_dir.join(mir_callgraph_bin_name());
    let version_file = bin_dir.join(".nightly-version");

    // Check if cached binary exists and nightly version matches
    if cached_bin.exists() {
        if let Ok(saved_ver) = std::fs::read_to_string(&version_file) {
            if saved_ver.trim() == nightly_ver {
                return Ok(cached_bin);
            }
            eprintln!("  [mir] nightly version changed, rebuilding mir-callgraph...");
        }
    } else {
        eprintln!("  [mir] building mir-callgraph (first run)...");
    }

    // Extract source
    let build_dir = base.join("build").join("mir-callgraph");
    extract_mir_callgraph_source(&build_dir)?;

    // Build with nightly
    eprintln!("  [mir] cargo +nightly build --release (this may take a minute)...");
    let status = Command::new("cargo")
        .args(["+nightly", "build", "--release"])
        .current_dir(&build_dir)
        .status()
        .context("failed to run cargo +nightly build")?;

    if !status.success() {
        bail!("mir-callgraph build failed (exit code: {status})");
    }

    // Copy built binary to bin/
    std::fs::create_dir_all(&bin_dir)
        .with_context(|| format!("failed to create bin dir: {}", bin_dir.display()))?;

    let built_bin = build_dir
        .join("target")
        .join("release")
        .join(mir_callgraph_bin_name());

    std::fs::copy(&built_bin, &cached_bin).with_context(|| {
        format!(
            "failed to copy built binary from {} to {}",
            built_bin.display(),
            cached_bin.display()
        )
    })?;

    // Save nightly version
    std::fs::write(&version_file, &nightly_ver)
        .context("failed to save nightly version")?;

    eprintln!("  [mir] mir-callgraph ready: {}", cached_bin.display());
    Ok(cached_bin)
}

/// Find the mir-callgraph binary.
///
/// Search order:
/// 1. Override path (explicit)
/// 2. Sibling to current exe
/// 3. Cached build in `~/.rude/bin/`
/// 4. Auto-build from embedded source
fn find_mir_callgraph_bin(override_path: Option<&Path>) -> Result<PathBuf> {
    // 1. Explicit override
    if let Some(p) = override_path {
        return Ok(p.to_path_buf());
    }

    // 2. Sibling to current exe
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(mir_callgraph_bin_name());
        if sibling.exists() {
            return Ok(sibling);
        }
    }

    // 3 & 4. Cached build or auto-build
    let base = rude_home()?;
    build_mir_callgraph(&base)
}

/// Derive the sqlite path for MIR data from a project root.
///
/// Returns `{project_root}/target/mir-edges/mir.db`.
pub fn mir_db_path(project_root: &Path) -> PathBuf {
    project_root.join("target").join("mir-edges").join("mir.db")
}

/// Clear mir_edges and mir_chunks tables for specific crates (or all if empty).
///
/// Must be called BEFORE running mir-callgraph so that lib + test compilations
/// can safely INSERT OR IGNORE without race conditions.
pub fn clear_mir_db(project_root: &Path, crates: &[&str]) -> Result<()> {
    let db_path = mir_db_path(project_root);
    if !db_path.exists() { return Ok(()); }
    let conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("failed to open mir.db: {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).ok();

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

/// Run mir-callgraph on the entire workspace.
pub fn run_mir_callgraph(project_root: &Path, mir_callgraph_bin: Option<&Path>) -> Result<MirEdgeMap> {
    run_mir_callgraph_for(project_root, mir_callgraph_bin, &[], false)
}

/// Run mir-callgraph for specific crates only (or all if `crates` is empty).
///
/// When `_rust_only` is true, Python/TypeScript extractors are skipped entirely.
pub fn run_mir_callgraph_for(
    project_root: &Path,
    mir_callgraph_bin: Option<&Path>,
    crates: &[&str],
    _rust_only: bool,
) -> Result<MirEdgeMap> {
    let out_dir = project_root.join("target").join("mir-edges");
    // No pre-deletion: RUSTC_WRAPPER now truncates on lib build and appends on test.
    // Crates that cargo skips (already built) keep their existing edge files intact.
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create MIR edge dir: {}", out_dir.display()))?;

    let bin = find_mir_callgraph_bin(mir_callgraph_bin)?;

    let mir_db = mir_db_path(project_root);

    let mut cmd = Command::new(&bin);
    add_nightly_path(&mut cmd);
    cmd.current_dir(project_root)
        .arg("--keep-going")
        .env("MIR_CALLGRAPH_OUT", &out_dir)
        .env("MIR_CALLGRAPH_DB", &mir_db)
        .env("MIR_CALLGRAPH_JSON", "1");

    for krate in crates {
        cmd.arg("-p").arg(krate);
    }

    let status = cmd.status()
        .with_context(|| format!("failed to run mir-callgraph: {}", bin.display()))?;

    if !status.success() {
        eprintln!("  [mir] mir-callgraph exited with {status} (partial results may be available)");
    }

    // Run language-specific extractors (Python, TypeScript)

    // Ensure rustc-args nightly version is saved so next run isn't stale.
    let args_dir = out_dir.join("rustc-args");
    if args_dir.exists() {
        let ver_file = args_dir.join(".nightly-version");
        if !ver_file.exists() {
            if let Some(ver) = nightly_rustc_version() {
                let _ = std::fs::write(&ver_file, &ver);
            }
        }
    }

    // Prefer sqlite if available, fallback to JSONL
    if mir_db.exists() {
        MirEdgeMap::from_sqlite(&mir_db, None)
    } else {
        MirEdgeMap::from_sqlite(&mir_db_path(project_root), None)
    }
}

/// Kill any background test processes from a previous `run_mir_direct` call.
///
/// Reads PIDs from `.test-bg.pid`, checks if they are still running, and
/// terminates them. The PID file is always removed afterwards.
fn kill_previous_test_bg(out_dir: &Path) {
    let pid_file = out_dir.join(".test-bg.pid");
    if !pid_file.exists() { return; }
    let content = match std::fs::read_to_string(&pid_file) {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = std::fs::remove_file(&pid_file);

    for line in content.lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            kill_process_by_pid(pid);
        }
    }
}

/// Check all `.edges.jsonl` and `.chunks.jsonl` files in `dir` for corrupt
/// trailing lines (non-empty line that isn't valid JSON). Delete corrupt files.
/// Returns true if the file's last non-empty line is not valid JSON.
/// Best-effort process kill by PID with image-name guard against PID reuse.
/// On Windows, uses `taskkill /FI "IMAGENAME eq mir-callgraph.exe"` to avoid
/// killing unrelated processes that inherited the same PID.
#[cfg(windows)]
fn kill_process_by_pid(pid: u32) {
    let _ = Command::new("taskkill")
        .args([
            "/F",
            "/PID", &pid.to_string(),
            "/FI", "IMAGENAME eq mir-callgraph.exe",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Best-effort process kill by PID with process-name guard against PID reuse.
/// On Unix, reads `/proc/{pid}/comm` to verify the process is `mir-callgraph`
/// before sending SIGKILL.
#[cfg(not(windows))]
fn kill_process_by_pid(pid: u32) {
    // Guard: only kill if the process is actually mir-callgraph.
    // If /proc is unavailable (e.g., macOS), fall through and kill anyway
    // since macOS PID reuse is less aggressive and the risk is lower.
    let comm_path = format!("/proc/{pid}/comm");
    if let Ok(name) = std::fs::read_to_string(&comm_path) {
        if !name.trim().starts_with("mir-callgraph") {
            return; // PID reused by a different process — do not kill
        }
    }
    let _ = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Pre-truncate MIR data for crates about to be rebuilt.
///
/// Deletes rows from sqlite (if available) and truncates JSONL files as fallback.
fn pre_truncate_crates(crates: &[&str], out_dir: &Path, _mir_db: &Path) {
    // SQLite: mir-callgraph now handles delta DELETE internally
    // (changed functions only, not whole crate), so no pre-truncate needed.

    // JSONL fallback: truncate files
    for krate in crates {
        let u = krate.replace('-', "_");
        for ext in [".edges.jsonl", ".chunks.jsonl"] {
            let p = out_dir.join(format!("{u}{ext}"));
            if p.exists() { let _ = std::fs::write(&p, ""); }
        }
    }
}

/// Run mir-callgraph in direct mode — always.
///
/// If prerequisites are missing (no cache, stale cache, missing artifacts),
/// runs cargo once to satisfy them, then executes direct mode.
/// Cargo is never used for MIR extraction itself, only for preparation.
/// When `_rust_only` is true, Python/TypeScript extractors are skipped entirely,
/// saving ~0.3s of directory-walk overhead.
/// Test targets are spawned in the background (fire-and-forget) and their PIDs
/// are recorded in `target/mir-edges/.test-bg.pid`. Only lib edges are included
/// in the returned `MirEdgeMap`.
pub fn run_mir_direct(
    project_root: &Path,
    mir_callgraph_bin: Option<&Path>,
    crates: &[&str],
    _rust_only: bool,
) -> Result<MirEdgeMap> {
    let out_dir = project_root.join("target").join("mir-edges");
    let args_dir = out_dir.join("rustc-args");

    // ── Phase 1: Ensure args cache is fresh ──────────────────────────
    let needs_cache_refresh = !args_dir.exists() || is_args_cache_stale(project_root, &args_dir);
    let needs_deps_rebuild = needs_cache_refresh || !all_extern_paths_valid(crates, &args_dir);

    if needs_deps_rebuild {
        if needs_cache_refresh {
            eprintln!("  [mir] refreshing rustc-args cache via cargo...");
        } else {
            eprintln!("  [mir] rebuilding deps (stale --extern artifacts)...");
        }
        // Run cargo with RUSTC_WRAPPER to (re)generate args cache + build deps.
        // This is preparation only — MIR extraction still happens via direct.
        run_mir_callgraph_for(project_root, mir_callgraph_bin, crates, _rust_only)?;

        // After cargo run, args cache is fresh. Continue to direct mode below
        // to ensure consistent extraction path.
    }

    // ── Phase 2: Collect args files (lib vs test separated) ─────────
    let mut lib_files = Vec::new();
    let mut test_files = Vec::new();
    for krate in crates {
        let crate_underscore = krate.replace('-', "_");
        let lib_file = args_dir.join(format!("{crate_underscore}.lib.rustc-args.json"));
        let test_file = args_dir.join(format!("{crate_underscore}.test.rustc-args.json"));
        if lib_file.exists() { lib_files.push(lib_file); }
        if test_file.exists() { test_files.push(test_file); }
    }

    let mir_db = mir_db_path(project_root);

    // If cargo just ran (Phase 1) and we still have no args, there's nothing
    // more we can do — the crate might not be a local crate.
    if lib_files.is_empty() && test_files.is_empty() {
        return if mir_db.exists() {
            MirEdgeMap::from_sqlite(&mir_db, None)
        } else {
            MirEdgeMap::from_sqlite(&mir_db_path(project_root), None)
        };
    }

    // If deps were just rebuilt via cargo (Phase 1), the edge files are already
    // generated by RUSTC_WRAPPER. Skip redundant direct extraction.
    if needs_deps_rebuild {
        return if mir_db.exists() {
            MirEdgeMap::from_sqlite(&mir_db, Some(crates))
        } else {
            MirEdgeMap::from_sqlite(&mir_db_path(project_root), None)
        };
    }

    // ── Phase 3: Direct mode — lib sync, test fire-and-forget ──────
    let bin = find_mir_callgraph_bin(mir_callgraph_bin)?;

    kill_previous_test_bg(&out_dir);

    pre_truncate_crates(crates, &out_dir, &mir_db);

    // ── Phase 3a: Launch lib builds synchronously ──────────────────
    let mut lib_children: Vec<(PathBuf, std::process::Child)> = Vec::new();
    let mut had_error = false;

    for args_file in &lib_files {
        let mut cmd = Command::new(&bin);
        add_nightly_path(&mut cmd);
        cmd.current_dir(project_root)
            .arg("--direct")
            .arg("--args-file").arg(args_file)
            .env("MIR_CALLGRAPH_OUT", &out_dir)
            .env("MIR_CALLGRAPH_DB", &mir_db)
            .env("MIR_CALLGRAPH_JSON", "1");
        if std::env::var("MIR_PROFILE").is_ok() {
            cmd.env("MIR_PROFILE", "1");
        }
        match cmd.spawn() {
            Ok(child) => lib_children.push((args_file.clone(), child)),
            Err(e) => {
                eprintln!("  [mir] failed to spawn direct (lib): {e}");
                had_error = true;
            }
        }
    }

    for (path, mut child) in lib_children {
        if let Ok(status) = child.wait() {
            if !status.success() {
                eprintln!("  [mir] direct failed for {}: {status}", path.display());
                had_error = true;
            }
        }
    }


    if had_error {
        eprintln!("  [mir] some lib builds failed, refreshing via cargo...");
        run_mir_callgraph_for(project_root, mir_callgraph_bin, crates, _rust_only)?;
        return if mir_db.exists() {
            MirEdgeMap::from_sqlite(&mir_db, Some(crates))
        } else {
            MirEdgeMap::from_sqlite(&mir_db_path(project_root), None)
        };
    }

    // ── Phase 3b: Fire-and-forget test builds ──────────────────────
    // Test processes append to the same JSONL files after lib is done.
    // We record their PIDs so the next `add` can wait/kill if needed.
    if !test_files.is_empty() {
        let mut test_pids: Vec<u32> = Vec::new();
        for args_file in &test_files {
            let mut cmd = Command::new(&bin);
            add_nightly_path(&mut cmd);
            cmd.current_dir(project_root)
                .arg("--direct")
                .arg("--args-file").arg(args_file)
                .env("MIR_CALLGRAPH_OUT", &out_dir)
                .env("MIR_CALLGRAPH_DB", &mir_db)
                .env("MIR_CALLGRAPH_JSON", "1");
            match cmd.spawn() {
                Ok(child) => {
                    test_pids.push(child.id());
                    // Intentionally leak the Child handle — the OS will reap
                    // the process when it exits. We track it via PID file.
                    std::mem::forget(child);
                }
                Err(e) => {
                    eprintln!("  [mir] failed to spawn direct (test): {e}");
                    // Non-fatal: lib edges are enough for now.
                }
            }
        }
        if !test_pids.is_empty() {
            let pid_file = out_dir.join(".test-bg.pid");
            let content = test_pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join("\n");
            let _ = std::fs::write(&pid_file, content);
            eprintln!("  [mir] test builds spawned in background (PIDs: {})",
                test_pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", "));
        }
    }

    // Prefer sqlite if available, fallback to JSONL.
    let result = if mir_db.exists() {
        MirEdgeMap::from_sqlite(&mir_db, Some(crates))
    } else {
        MirEdgeMap::from_sqlite(&mir_db_path(project_root), Some(crates))
    };
    result
}

/// Check if all requested crates have valid --extern artifact paths.
fn all_extern_paths_valid(crates: &[&str], args_dir: &Path) -> bool {
    for krate in crates {
        let crate_underscore = krate.replace('-', "_");
        for suffix in [".lib", ".test"] {
            let f = args_dir.join(format!("{crate_underscore}{suffix}.rustc-args.json"));
            if f.exists() && !validate_extern_paths(&f) {
                return false;
            }
        }
    }
    true
}

// Edge file cleanup removed: RUSTC_WRAPPER now truncates on lib build.
// Crates skipped by cargo keep their existing (valid) edge files.

/// Check if the rustc-args cache is stale.
///
/// Stale conditions:
/// 1. Cargo.toml or Cargo.lock modified after the cache directory
/// 2. Nightly rustc version changed
/// Check if --extern artifact paths in a cached args file still exist.
/// If any .rlib/.rmeta/.dll/.so is missing, direct mode will fail.
fn validate_extern_paths(args_file: &Path) -> bool {
    let content = match std::fs::read_to_string(args_file) {
        Ok(c) => c,
        Err(_) => return false,
    };
    // Parse JSON to get the args array with proper string unescaping.
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&content);
    let args = match parsed {
        Ok(v) => v.get("args")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default(),
        Err(_) => return false,
    };
    for arg in &args {
        let Some(s) = arg.as_str() else { continue };
        let is_artifact = s.ends_with(".rlib") || s.ends_with(".rmeta")
            || s.ends_with(".dll") || s.ends_with(".so") || s.ends_with(".dylib");
        if !is_artifact { continue; }
        // --extern name=path or just a path
        let path_str = s.rsplit('=').next().unwrap_or(s);
        if !std::path::Path::new(path_str).exists() {
            return false;
        }
    }
    true
}

fn is_args_cache_stale(project_root: &Path, args_dir: &Path) -> bool {
    // Get the oldest cache file mtime as reference
    let cache_mtime = match args_dir_oldest_mtime(args_dir) {
        Some(t) => t,
        None => return true, // no cache files
    };

    // Check Cargo.toml
    let cargo_toml = project_root.join("Cargo.toml");
    if let Ok(meta) = std::fs::metadata(&cargo_toml) {
        if let Ok(mtime) = meta.modified() {
            if mtime > cache_mtime {
                return true;
            }
        }
    }

    // Check Cargo.lock (dependency changes, feature changes via lock update)
    let cargo_lock = project_root.join("Cargo.lock");
    if let Ok(meta) = std::fs::metadata(&cargo_lock) {
        if let Ok(mtime) = meta.modified() {
            if mtime > cache_mtime {
                return true;
            }
        }
    }

    // Check all workspace member Cargo.toml files (feature flag changes, dep edits).
    // For single-crate projects, parse_workspace_members returns empty — the root
    // Cargo.toml is already checked above.
    if let Ok(root_content) = std::fs::read_to_string(&cargo_toml) {
        for member_dir in parse_workspace_members(&root_content, project_root) {
            let member_toml = project_root.join(&member_dir).join("Cargo.toml");
            if let Ok(meta) = std::fs::metadata(&member_toml) {
                if let Ok(mtime) = meta.modified() {
                    if mtime > cache_mtime {
                        return true;
                    }
                }
            }
        }
    }

    // Check .cargo/config.toml (target changes, rustflags, etc.)
    for config_name in [".cargo/config.toml", ".cargo/config"] {
        let config_path = project_root.join(config_name);
        if let Ok(meta) = std::fs::metadata(&config_path) {
            if let Ok(mtime) = meta.modified() {
                if mtime > cache_mtime {
                    return true;
                }
            }
        }
    }

    // Check nightly version
    let version_file = args_dir.join(".nightly-version");
    if let Some(current_ver) = nightly_rustc_version() {
        match std::fs::read_to_string(&version_file) {
            Ok(saved) if saved.trim() == current_ver => {}
            _ => {
                // Save current version for next check
                let _ = std::fs::write(&version_file, &current_ver);
                return true;
            }
        }
    }

    false
}

/// Get the oldest modification time among `.rustc-args.json` files in a directory.
fn args_dir_oldest_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut oldest: Option<std::time::SystemTime> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.to_string_lossy().ends_with(".rustc-args.json") {
                if let Ok(meta) = std::fs::metadata(&path) {
                    if let Ok(mtime) = meta.modified() {
                        oldest = Some(match oldest {
                            Some(prev) if prev < mtime => prev,
                            _ => mtime,
                        });
                    }
                }
            }
        }
    }
    oldest
}

/// Detect which crates contain the given changed files.
///
/// Walks up from each file to find the nearest Cargo.toml, then extracts the package name.
pub fn detect_changed_crates(project_root: &Path, changed_files: &[impl AsRef<Path>]) -> Vec<String> {
    let mut crates = std::collections::HashSet::new();
    for file in changed_files {
        let file = file.as_ref();
        let abs = if file.is_absolute() {
            file.to_path_buf()
        } else {
            project_root.join(file)
        };
        // Walk up to find Cargo.toml
        let mut dir = abs.parent();
        while let Some(d) = dir {
            let cargo_toml = d.join("Cargo.toml");
            if cargo_toml.exists() {
                // Extract package name from Cargo.toml
                if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("name") {
                            if let Some(name) = trimmed.split('"').nth(1) {
                                crates.insert(name.to_owned());
                            }
                            break;
                        }
                    }
                }
                break;
            }
            dir = d.parent();
        }
    }
    crates.into_iter().collect()
}

/// A chunk definition extracted from MIR — function/struct/enum with location info.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct MirChunk {
    pub name: String,
    pub file: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub is_test: bool,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub calls: String,
    #[serde(default)]
    pub type_refs: String,
}

/// Convert MirChunks directly to ParsedChunks, skipping text format intermediary.
pub fn mir_chunks_to_parsed(mir_chunks: &[MirChunk]) -> Vec<crate::parse::ParsedChunk> {
    mir_chunks
        .iter()
        .map(|mc| {
            let kind = match mc.kind.as_str() {
                "fn" | "method" => "function".to_string(),
                other => other.to_string(),
            };

            let mut calls = Vec::new();
            let mut call_lines = Vec::new();
            if !mc.calls.is_empty() {
                for entry in mc.calls.split(", ") {
                    if let Some(at_pos) = entry.rfind('@') {
                        let callee = entry[..at_pos].to_string();
                        let line: u32 = entry[at_pos + 1..].parse().unwrap_or(0);
                        calls.push(callee);
                        call_lines.push(line);
                    } else {
                        calls.push(entry.to_string());
                        call_lines.push(0);
                    }
                }
            }

            let types: Vec<String> = if mc.type_refs.is_empty() {
                Vec::new()
            } else {
                mc.type_refs.split(", ").map(|s| s.to_string()).collect()
            };

            crate::parse::ParsedChunk {
                kind,
                name: mc.name.clone(),
                file: mc.file.clone(),
                lines: Some((mc.start_line, mc.end_line)),
                signature: mc.signature.clone(),
                calls,
                call_lines,
                types,
                imports: Vec::new(),
                string_args: Vec::new(),
                param_flows: Vec::new(),
                param_types: Vec::new(),
                field_types: Vec::new(),
                local_types: Vec::new(),
                let_call_bindings: Vec::new(),
                return_type: None,
                field_accesses: Vec::new(),
                enum_variants: Vec::new(),
                is_test: mc.is_test,
            }
        })
        .collect()
}

/// Load MIR chunks from a JSONL file.
/// Load all chunks from `.chunks.jsonl` files in a directory.
/// Load MIR chunks, optionally filtering to specific crates only.
/// Detect workspace crates whose `.edges.jsonl` files are missing from the MIR output dir.
///
/// Reads the root `Cargo.toml` `[workspace] members` list, extracts each member's
/// package name, then checks if `target/mir-edges/{crate_name}.edges.jsonl` exists.
/// Returns the names of crates with missing edge files.
pub fn detect_missing_edge_crates(project_root: &Path) -> Vec<String> {
    let edge_dir = project_root.join("target").join("mir-edges");
    let workspace_toml = project_root.join("Cargo.toml");

    let content = match std::fs::read_to_string(&workspace_toml) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    // Parse workspace members; for single-crate (non-workspace) projects,
    // treat the project root as the sole member.
    let member_dirs = parse_workspace_members(&content, project_root);
    let is_single_crate = member_dirs.is_empty();

    let check_list: Vec<(std::path::PathBuf, std::path::PathBuf)> = if is_single_crate {
        // Single crate: root Cargo.toml + root src/
        vec![(workspace_toml.clone(), project_root.join("src"))]
    } else {
        member_dirs.iter()
            .map(|d| (project_root.join(d).join("Cargo.toml"), project_root.join(d).join("src")))
            .collect()
    };

    // Pre-load crate names that exist in sqlite (if available).
    let mir_db = mir_db_path(project_root);
    let sqlite_crates: std::collections::HashSet<String> = if mir_db.exists() {
        rusqlite::Connection::open(&mir_db)
            .and_then(|conn| {
                let mut stmt = conn.prepare("SELECT DISTINCT crate_name FROM mir_edges")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<std::result::Result<std::collections::HashSet<String>, _>>()
            })
            .unwrap_or_default()
    } else {
        std::collections::HashSet::new()
    };

    let mut missing = Vec::new();
    for (toml_path, src_dir) in &check_list {
        let pkg_name = match std::fs::read_to_string(toml_path) {
            Ok(c) => extract_package_name(&c),
            Err(_) => continue,
        };
        let Some(name) = pkg_name else { continue };

        if !src_dir.exists() {
            continue;
        }

        let cn = name.replace('-', "_");

        // Present in sqlite → not missing
        if sqlite_crates.contains(&cn) {
            continue;
        }

        // Skip crates without lib args (bin-only crates like the main binary)
        let args_dir = edge_dir.join("rustc-args");
        let lib_args = args_dir.join(format!("{cn}.lib.rustc-args.json"));
        if args_dir.exists() && !lib_args.exists() {
            continue;
        }

        // Fallback: check JSONL
        let edge_file = edge_dir.join(format!("{cn}.edges.jsonl"));
        if !edge_file.exists() {
            missing.push(name);
        }
    }
    missing
}

/// Parse `[workspace] members = [...]` from a Cargo.toml string.
fn parse_workspace_members_raw(content: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut in_members = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("members") && trimmed.contains('[') {
            in_members = true;
            // Handle inline items on the same line as `members = [`
            for part in trimmed.split('[').nth(1).into_iter() {
                for item in part.split(']').next().into_iter() {
                    for entry in item.split(',') {
                        let entry = entry.trim().trim_matches('"').trim();
                        if !entry.is_empty() {
                            members.push(entry.to_owned());
                        }
                    }
                }
            }
            if trimmed.contains(']') {
                in_members = false;
            }
            continue;
        }
        if in_members {
            if trimmed.contains(']') {
                // Last line of the array
                let before_bracket = trimmed.split(']').next().unwrap_or("");
                let entry = before_bracket.trim().trim_matches(',').trim().trim_matches('"').trim();
                if !entry.is_empty() {
                    members.push(entry.to_owned());
                }
                in_members = false;
                continue;
            }
            // Strip comments
            let no_comment = trimmed.split('#').next().unwrap_or(trimmed);
            let entry = no_comment.trim().trim_matches(',').trim().trim_matches('"').trim();
            if !entry.is_empty() {
                members.push(entry.to_owned());
            }
        }
    }
    members
}

/// Parse `[workspace] members = [...]` from a Cargo.toml string,
/// expanding glob patterns like `crates/*` via directory listing.
fn parse_workspace_members(content: &str, project_root: &Path) -> Vec<String> {
    let raw = parse_workspace_members_raw(content);
    let mut expanded = Vec::new();
    for entry in raw {
        if entry.contains('*') {
            // Expand simple trailing `/*` glob: list child directories that contain Cargo.toml.
            // Supports patterns like "crates/*" or "packages/*".
            let prefix = entry.trim_end_matches('*').trim_end_matches('/');
            let parent_dir = project_root.join(prefix);
            if let Ok(read_dir) = std::fs::read_dir(&parent_dir) {
                let mut children: Vec<String> = Vec::new();
                for dir_entry in read_dir.flatten() {
                    let path = dir_entry.path();
                    if path.is_dir() && path.join("Cargo.toml").exists() {
                        if let Ok(rel) = path.strip_prefix(project_root) {
                            children.push(rel.to_string_lossy().replace('\\', "/"));
                        }
                    }
                }
                children.sort();
                expanded.extend(children);
            }
        } else {
            expanded.push(entry);
        }
    }
    expanded
}

/// Extract `name = "..."` from the `[package]` section of a Cargo.toml.
fn extract_package_name(content: &str) -> Option<String> {
    let mut in_package = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package && trimmed.starts_with("name") {
            return trimmed.split('"').nth(1).map(|s| s.to_owned());
        }
    }
    None
}

