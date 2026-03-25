
use std::path::Path;

use super::runner::nightly_rustc_version;
use super::sqlite::mir_db_path;

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
