
use std::path::Path;

use super::runner::nightly_rustc_version;
use super::sqlite::mir_db_path;

pub fn detect_changed_crates(project_root: &Path, changed_files: &[impl AsRef<Path>]) -> Vec<String> {
    let mut crates = std::collections::HashSet::new();
    for file in changed_files {
        let abs = if file.as_ref().is_absolute() { file.as_ref().to_path_buf() } else { project_root.join(file) };
        let mut dir = abs.parent();
        while let Some(d) = dir {
            let toml = d.join("Cargo.toml");
            if toml.exists() {
                if let Some(name) = std::fs::read_to_string(&toml).ok().and_then(|c| extract_package_name(&c)) {
                    crates.insert(name);
                }
                break;
            }
            dir = d.parent();
        }
    }
    crates.into_iter().collect()
}

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

pub(super) fn all_extern_paths_valid(crates: &[&str], args_dir: &Path) -> bool {
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

pub(super) fn is_args_cache_stale(project_root: &Path, args_dir: &Path) -> bool {
    let cache_mtime = match args_dir_oldest_mtime(args_dir) {
        Some(t) => t,
        None => return true,
    };
    let is_newer = |p: &Path| -> bool {
        std::fs::metadata(p).ok()
            .and_then(|m| m.modified().ok())
            .is_some_and(|t| t > cache_mtime)
    };
    let cargo_toml = project_root.join("Cargo.toml");
    if is_newer(&cargo_toml) || is_newer(&project_root.join("Cargo.lock")) { return true; }
    if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
        if parse_workspace_members(&content, project_root).iter()
            .any(|d| is_newer(&project_root.join(d).join("Cargo.toml"))) { return true; }
    }
    if [".cargo/config.toml", ".cargo/config"].iter()
        .any(|c| is_newer(&project_root.join(c))) { return true; }
    let version_file = args_dir.join(".nightly-version");
    if let Some(ver) = nightly_rustc_version() {
        match std::fs::read_to_string(&version_file) {
            Ok(saved) if saved.trim() == ver => {}
            _ => { let _ = std::fs::write(&version_file, &ver); return true; }
        }
    }
    false
}

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

fn parse_workspace_members_raw(content: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut in_members = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("members") && trimmed.contains('[') {
            in_members = true;
        }
        if !in_members { continue; }
        let chunk = trimmed.split('#').next().unwrap_or(trimmed);
        let chunk = chunk.split('[').last().unwrap_or(chunk);
        let chunk = chunk.split(']').next().unwrap_or(chunk);
        for entry in chunk.split(',') {
            let e = entry.trim().trim_matches('"').trim();
            if !e.is_empty() && e != "members" && !e.contains('=') { members.push(e.to_owned()); }
        }
        if trimmed.contains(']') { in_members = false; }
    }
    members
}

pub(super) fn parse_workspace_members(content: &str, project_root: &Path) -> Vec<String> {
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

pub(super) fn extract_package_name(content: &str) -> Option<String> {
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
