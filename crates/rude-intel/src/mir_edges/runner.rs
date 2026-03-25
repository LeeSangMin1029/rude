//! mir-callgraph subprocess execution and binary management.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use super::sqlite::mir_db_path;
use super::types::MirEdgeMap;
use super::workspace::{all_extern_paths_valid, is_args_cache_stale};

// Embedded mir-callgraph source files for auto-build.
const MIR_CALLGRAPH_MAIN_RS: &str =
    include_str!("../../../../tools/mir-callgraph/src/main.rs");
const MIR_CALLGRAPH_CARGO_TOML: &str =
    include_str!("../../../../tools/mir-callgraph/Cargo.toml");
const MIR_CALLGRAPH_RUST_TOOLCHAIN: &str =
    include_str!("../../../../tools/mir-callgraph/rust-toolchain.toml");

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
pub(super) fn nightly_rustc_version() -> Option<String> {
    Command::new("rustc")
        .args(["+nightly", "--version"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
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
pub(super) fn add_nightly_path(cmd: &mut Command) {
    if let Some(nightly_bin) = nightly_sysroot_bin() {
        let current = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{current};{nightly_bin}"));
    }
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

    MirEdgeMap::from_sqlite(&mir_db, None)
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
        return MirEdgeMap::from_sqlite(&mir_db, None);
    }

    // If deps were just rebuilt via cargo (Phase 1), the edge files are already
    // generated by RUSTC_WRAPPER. Skip redundant direct extraction.
    if needs_deps_rebuild {
        return MirEdgeMap::from_sqlite(&mir_db, Some(crates));
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
        return MirEdgeMap::from_sqlite(&mir_db, Some(crates));
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

    MirEdgeMap::from_sqlite(&mir_db, Some(crates))
}
