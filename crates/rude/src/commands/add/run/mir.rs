use std::path::PathBuf;

use anyhow::{Context, Result};

use super::scan::prof;

pub fn run_mir_analysis(
    input_path: &std::path::Path,
    mir_db: &std::path::Path,
    code_files: &[&PathBuf],
    missing_crates: &[String],
) -> Result<Vec<String>> {
    let out_dir = input_path.join("target").join("mir-edges");
    rude_intel::mir_edges::check_bin_version_match(&out_dir, None);
    let has_cached_edges = mir_db.exists() && std::fs::metadata(mir_db).map_or(false, |m| m.len() > 0);

    if !has_cached_edges {
        rude_intel::mir_edges::clear_mir_db(input_path, &[]).ok();
        run_mir_cargo_wrapper(input_path)?;
        return Ok(Vec::new());
    }

    let rust_changed: Vec<_> = code_files.iter()
        .filter(|f| f.extension().and_then(|e| e.to_str()) == Some("rs"))
        .collect();
    let mut changed_crates = prof!("detect_changed_crates", rude_intel::mir_edges::detect_changed_crates(input_path, &rust_changed));
    for m in missing_crates {
        if !changed_crates.iter().any(|c| c == m) { changed_crates.push(m.clone()); }
    }
    if changed_crates.is_empty() { return Ok(Vec::new()); }

    let crate_refs: Vec<&str> = changed_crates.iter().map(|s| s.as_str()).collect();
    tracing::debug!("[mir] incremental: {} crate(s) — {}", crate_refs.len(), crate_refs.join(", "));
    let truly_changed: Vec<&str> = crate_refs.iter()
        .filter(|c| !missing_crates.iter().any(|m| m == *c))
        .copied().collect();
    if !truly_changed.is_empty() {
        prof!("clear_mir_db", rude_intel::mir_edges::clear_mir_db(input_path, &truly_changed).ok());
        let rust_only = code_files.iter().all(|f| f.extension().and_then(|e| e.to_str()) == Some("rs"));
        prof!("run_mir_direct", rude_intel::mir_edges::run_mir_direct(input_path, None, &truly_changed, rust_only)
            .context("mir-callgraph incremental failed")?);
    }
    // missing crates: check if they have mir.db data, if not do full cargo check
    let missing_without_data: Vec<&str> = missing_crates.iter()
        .filter(|m| {
            let mir_db = rude_intel::mir_edges::mir_db_path(input_path);
            !mir_db.exists() || {
                let filter = [m.as_str()];
                let edge_map = rude_intel::mir_edges::MirEdgeMap::from_sqlite(&mir_db, Some(&filter)).unwrap_or_default();
                edge_map.total == 0
            }
        })
        .map(|s| s.as_str()).collect();
    if !missing_without_data.is_empty() {
        tracing::debug!("[mir] full rebuild for missing crates: {}", missing_without_data.join(", "));
        rude_intel::mir_edges::run_mir_callgraph(input_path, None)
            .context("mir-callgraph full rebuild failed")?;
    }
    Ok(changed_crates)
}

pub fn run_sub_workspaces(
    root: &std::path::Path, main_mir_db: &std::path::Path, code_files: &[&PathBuf],
) -> Result<()> {
    let sub_workspaces = find_sub_workspaces(root);
    let abs_root = rude_util::safe_canonicalize(root);
    for ws in &sub_workspaces {
        let abs_ws = rude_util::safe_canonicalize(ws);
        let ws_mir_db = abs_ws.join("target").join("mir-edges").join("mir.db");
        let has_changes = code_files.iter().any(|f| {
            let abs_f = if f.is_absolute() { f.to_path_buf() } else { abs_root.join(f) };
            abs_f.starts_with(&abs_ws)
        });
        if has_changes {
            tracing::debug!("[mir] sub-workspace: {}", ws.display());
            let ws_args_dir = abs_ws.join("target").join("mir-edges").join("rustc-args");
            if ws_args_dir.exists() {
                let changed_ws: Vec<PathBuf> = code_files.iter().filter_map(|f| {
                    let abs_f = if f.is_absolute() { f.to_path_buf() } else { abs_root.join(f) };
                    abs_f.strip_prefix(&abs_ws).ok().map(|rel| abs_ws.join(rel))
                }).collect();
                let refs: Vec<&PathBuf> = changed_ws.iter().collect();
                let crates = rude_intel::mir_edges::detect_changed_crates(&abs_ws, &refs);
                if !crates.is_empty() {
                    let refs: Vec<&str> = crates.iter().map(|s| s.as_str()).collect();
                    rude_intel::mir_edges::clear_mir_db(&abs_ws, &refs).ok();
                    rude_intel::mir_edges::run_mir_direct(&abs_ws, None, &refs, true).ok();
                }
            } else {
                run_mir_cargo_wrapper(&abs_ws).ok();
            }
        } else if !ws_mir_db.exists() {
            run_mir_cargo_wrapper(&abs_ws).ok();
        }
        if ws_mir_db.exists() {
            rude_intel::mir_edges::merge_mir_db(main_mir_db, &ws_mir_db, &abs_root, &abs_ws).ok();
        }
    }
    Ok(())
}

fn run_mir_cargo_wrapper(ws: &std::path::Path) -> Result<()> {
    let bin = rude_intel::mir_edges::find_mir_callgraph_bin(None)?;
    let out_dir = ws.join("target").join("mir-edges");
    std::fs::create_dir_all(&out_dir).ok();
    let abs_out = std::fs::canonicalize(&out_dir).unwrap_or_else(|_| out_dir.clone());
    let abs_out = rude_util::strip_unc_prefix_path(&abs_out);
    let abs_db = abs_out.join("mir.db");
    let abs_bin = std::fs::canonicalize(&bin).unwrap_or_else(|_| bin.clone());
    let abs_bin = rude_util::strip_unc_prefix_path(&abs_bin);
    let members = detect_workspace_members(ws);
    let run = |extra_args: &[&str]| {
        let mir_target = ws.join("target").join(rude_intel::mir_edges::mir_check_dir_name());
        let mut cmd = std::process::Command::new("cargo");
        cmd.arg("check")
            .env("RUSTUP_TOOLCHAIN", "nightly")
            .arg("--target-dir").arg(&mir_target)
            .current_dir(ws)
            .env("RUSTC_WRAPPER", &abs_bin)
            .env("MIR_CALLGRAPH_OUT", &abs_out)
            .env("MIR_CALLGRAPH_DB", &abs_db)
            .env("MIR_CALLGRAPH_JSON", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit());
        for m in &members { cmd.arg("-p").arg(m); }
        for a in extra_args { cmd.arg(a); }
        rude_intel::mir_edges::runner::add_nightly_path(&mut cmd);
        cmd.status()
    };
    let lib_status = run(&[]).context("cargo check (lib) failed")?;
    if !lib_status.success() {
        let db_size = std::fs::metadata(&abs_db).map(|m| m.len()).unwrap_or(0);
        if db_size == 0 {
            eprintln!("  [mir] cargo check failed. Trying individual packages...");
            for m in &members {
                let _ = run(&[&format!("-p={m}")]);
            }
        }
    }
    let db_size = std::fs::metadata(&abs_db).map(|m| m.len()).unwrap_or(0);
    if db_size > 0 {
        let test_status = run(&["--tests"]);
        if let Ok(s) = test_status {
            if !s.success() {
                eprintln!("  [mir] cargo check (tests) partial: {s}");
            }
        }
    }
    Ok(())
}

fn detect_workspace_members(root: &std::path::Path) -> Vec<String> {
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output().ok();
    let Some(out) = output.filter(|o| o.status.success()) else { return Vec::new() };
    let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Vec::new() };
    meta.get("packages").and_then(|p| p.as_array())
        .map(|pkgs| pkgs.iter().filter_map(|p| p.get("name")?.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

pub fn find_sub_workspaces(root: &std::path::Path) -> Vec<PathBuf> {
    let cache_file = root.join("target").join("mir-edges").join(".sub-workspaces");
    let toml_mtime = std::fs::metadata(root.join("Cargo.toml"))
        .and_then(|m| m.modified()).ok();
    let cache_mtime = std::fs::metadata(&cache_file)
        .and_then(|m| m.modified()).ok();
    if let (Some(t), Some(c)) = (toml_mtime, cache_mtime) {
        if c > t {
            if let Ok(content) = std::fs::read_to_string(&cache_file) {
                return content.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect();
            }
        }
    }
    let result = detect_sub_workspaces(root);
    let text: String = result.iter().filter_map(|p| p.to_str()).collect::<Vec<_>>().join("\n");
    let _ = std::fs::write(&cache_file, text);
    result
}

fn detect_sub_workspaces(root: &std::path::Path) -> Vec<PathBuf> {
    let meta_output = std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root).output().ok();
    let Some(out) = meta_output.filter(|o| o.status.success()) else { return Vec::new() };
    let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Vec::new() };
    let norm = |s: &str| s.replace('\\', "/").to_lowercase();
    let ws_root = meta.get("workspace_root").and_then(|v| v.as_str()).map(norm).unwrap_or_default();
    let members: std::collections::HashSet<String> = meta.get("packages")
        .and_then(|p| p.as_array())
        .map(|pkgs| pkgs.iter().filter_map(|p| {
            Some(norm(&PathBuf::from(p.get("manifest_path")?.as_str()?).parent()?.to_string_lossy()))
        }).collect())
        .unwrap_or_default();
    let git_output = std::process::Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "*/Cargo.toml"])
        .current_dir(root).output().ok();
    let Some(git_out) = git_output.filter(|o| o.status.success()) else { return Vec::new() };
    let abs_root = rude_util::safe_canonicalize(root);
    String::from_utf8_lossy(&git_out.stdout).lines()
        .filter_map(|line| {
            let parent = PathBuf::from(line).parent()?.to_path_buf();
            if parent.as_os_str().is_empty() { return None; }
            let abs_dir = rude_util::safe_canonicalize(&abs_root.join(&parent));
            let dir_norm = norm(&abs_dir.to_string_lossy());
            if members.contains(&dir_norm) || dir_norm == ws_root { return None; }
            let candidate = root.join(&parent);
            let valid = std::process::Command::new("cargo")
                .args(["metadata", "--no-deps", "--format-version", "1"])
                .current_dir(&candidate)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status().map(|s| s.success()).unwrap_or(false);
            if !valid {
                tracing::debug!("[mir] skipping broken sub-workspace: {}", candidate.display());
                return None;
            }
            Some(candidate)
        })
        .collect()
}

pub fn to_crate_filter(crates: &[String]) -> Option<Vec<&str>> {
    if crates.is_empty() { None } else { Some(crates.iter().map(String::as_str).collect()) }
}
