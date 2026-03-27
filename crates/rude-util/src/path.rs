use std::path::{Path, PathBuf};

pub fn strip_unc_prefix(path: &str) -> &str {
    path.strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix("//?/"))
        .unwrap_or(path)
}

pub fn strip_unc_prefix_path(path: &Path) -> PathBuf {
    PathBuf::from(strip_unc_prefix(&path.to_string_lossy()))
}

pub fn safe_canonicalize(path: &Path) -> PathBuf {
    strip_unc_prefix_path(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
}

pub fn normalize_source(path: &Path) -> String {
    safe_canonicalize(path).to_string_lossy().replace('\\', "/")
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

pub fn find_project_root(db: &Path) -> Option<PathBuf> {
    let abs = safe_canonicalize(db);
    let start = if abs.is_dir() { abs } else { abs.parent()?.to_path_buf() };
    let markers = ["Cargo.toml", ".git", "pyproject.toml", "setup.py", "go.mod", "package.json", "tsconfig.json"];
    let mut dir = start.as_path();
    for _ in 0..10 {
        if markers.iter().any(|m| dir.join(m).exists()) {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
    None
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
