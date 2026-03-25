
use std::path::{Path, PathBuf};

use crate::data::parse::ParsedChunk;
pub fn find_project_root(db: &Path) -> Option<PathBuf> {
    let abs = db.canonicalize().ok()?;
    let start = if abs.is_dir() {
        abs
    } else {
        abs.parent()?.to_path_buf()
    };
    let markers = [
        "Cargo.toml",
        ".git",
        "pyproject.toml",
        "setup.py",
        "go.mod",
        "package.json",
        "tsconfig.json",
    ];
    let mut dir = start.as_path();
    for _ in 0..10 {
        if markers.iter().any(|m| dir.join(m).exists()) {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
    None
}

pub fn format_lines_str_opt(lines: Option<(usize, usize)>) -> String {
    if let Some((s, e)) = lines {
        format!("{s}-{e}")
    } else {
        String::new()
    }
}

pub fn format_lines_opt(lines: Option<(usize, usize)>) -> String {
    let s = format_lines_str_opt(lines);
    if s.is_empty() { s } else { format!(":{s}") }
}

pub fn relative_path(path: &str) -> &str {
    if let Some(idx) = path.find("crates/") {
        &path[idx..]
    } else if let Some(idx) = path.find("src/") {
        &path[idx..]
    } else {
        path
    }
}

pub fn build_path_aliases(paths: &[&str]) -> (std::collections::BTreeMap<String, String>, Vec<(String, String)>) {
    use std::collections::{BTreeMap, BTreeSet};

    // Collect unique directories.
    let mut dirs: BTreeSet<&str> = BTreeSet::new();
    for p in paths {
        let dir = match p.rfind('/') {
            Some(i) => &p[..=i],
            None => "",
        };
        if !dir.is_empty() {
            dirs.insert(dir);
        }
    }

    // Group directories by crate root (crates/<name>/src/).
    // Key: crate root prefix (e.g. "crates/rude/src/"), Value: subdirectory suffixes
    let mut crate_dirs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for &dir in &dirs {
        if let Some(root) = extract_crate_src_root(dir) {
            let suffix = &dir[root.len()..];
            crate_dirs.entry(root.to_owned()).or_default().insert(suffix.to_owned());
        }
        // Non-crate directories (no src/) are skipped — not code paths.
    }

    let mut alias_map: BTreeMap<String, String> = BTreeMap::new();
    let mut legend: Vec<(String, String)> = Vec::new();
    let mut label = b'A';

    // Assign crate aliases.
    for (root, subdirs) in &crate_dirs {
        if label > b'Z' { break; }
        let letter = label as char;

        // Bare letter = crate root (src/)
        let root_alias = format!("[{letter}]");
        alias_map.insert(root.clone(), root_alias.clone());
        legend.push((root_alias, root.clone()));

        // Numbered suffixes for subdirectories
        let mut sub_num = 1u32;
        for subdir in subdirs {
            if subdir.is_empty() { continue; } // src/ itself, already handled
            let full_dir = format!("{root}{subdir}");
            let sub_alias = format!("[{letter}{sub_num}]");
            alias_map.insert(full_dir, sub_alias.clone());
            legend.push((sub_alias, format!("  {subdir}")));
            sub_num += 1;
        }

        label += 1;
    }

    (alias_map, legend)
}

fn extract_crate_src_root(dir: &str) -> Option<&str> {
    let crate_start = dir.find("crates/")?;
    let after_crates = &dir[crate_start + 7..];
    let name_end = after_crates.find('/')?;
    let after_name = &after_crates[name_end + 1..];
    // Expect "src/" after crate name
    if after_name.starts_with("src/") {
        let root_end = crate_start + 7 + name_end + 1 + 4; // "src/"
        Some(&dir[..root_end])
    } else {
        None
    }
}

pub fn apply_alias(path: &str, alias_map: &std::collections::BTreeMap<String, String>) -> String {
    // Find the longest matching directory prefix.
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => return path.to_owned(),
    };
    if let Some(alias) = alias_map.get(dir) {
        let file = &path[dir.len()..];
        format!("{alias}{file}")
    } else {
        path.to_owned()
    }
}

pub fn lines_str(c: &ParsedChunk) -> String {
    if let Some((start, end)) = c.lines {
        format!("{start}-{end}")
    } else {
        String::new()
    }
}

pub fn extract_crate_name(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    if let Some(start) = normalized.find("crates/") {
        let rest = &normalized[start + 7..];
        if let Some(slash) = rest.find('/') {
            return rest[..slash].to_owned();
        }
    }
    "(root)".to_owned()
}
