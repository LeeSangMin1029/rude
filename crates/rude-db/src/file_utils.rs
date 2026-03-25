//! File path, hashing, and scanning utilities.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Cross-platform home directory (`USERPROFILE` on Windows, `HOME` elsewhere).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Strip Windows extended-length prefix (`\\?\` or `//?/`).
///
/// `canonicalize()` on Windows adds this prefix, which breaks `git ls-files`,
/// shell commands, and path comparison.
pub fn strip_unc_prefix(path: &str) -> &str {
    path.strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix("//?/"))
        .unwrap_or(path)
}

/// Like [`strip_unc_prefix`] but returns a `PathBuf`.
pub fn strip_unc_prefix_path(path: &Path) -> PathBuf {
    PathBuf::from(strip_unc_prefix(&path.to_string_lossy()))
}

/// Normalize a file path to a canonical forward-slash form.
///
/// Resolves symlinks, strips Windows `\\?\` prefix, normalizes separators.
pub fn normalize_source(path: &Path) -> String {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy();
    strip_unc_prefix(&s).replace('\\', "/")
}

/// Generate a stable ID from source path and chunk index.
pub fn generate_id(source: &str, chunk_index: usize) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    chunk_index.hash(&mut hasher);
    hasher.finish()
}

/// Compute a content hash (MD5 -> u64) for a file's raw bytes.
///
/// Used for change detection: if mtime/size changed but content hash
/// is identical, we skip expensive re-processing.
pub fn content_hash(path: &Path) -> Result<u64> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read file for hashing: {}", path.display()))?;
    Ok(content_hash_bytes(&bytes))
}

/// Compute content hash from raw bytes (DefaultHasher → u64).
pub fn content_hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Map file extension to language name. Returns `"other"` for unsupported extensions.
pub fn lang_for_ext(ext: &str) -> &'static str {
    match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" | "pyi" => "python",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "hpp" | "cc" | "cxx" | "hxx" | "hh" => "cpp",
        _ => "other",
    }
}

/// Check if a file extension is a supported code file.
pub fn is_code_ext(ext: &str) -> bool {
    lang_for_ext(ext) != "other"
}

/// Get the file modification time as seconds since UNIX epoch.
pub fn get_file_mtime(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// Built-in directory names always skipped during file scanning.
const BUILTIN_SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    ".swarm",
    "__pycache__",
    ".venv",
    "dist",
    "vendor",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".claude",
    "build",
    "mutants.out",
];

/// Check if a directory entry should be skipped during walkdir scanning.
///
/// Skips built-in cache/build directories and any user-specified `--exclude` dirs.
pub fn should_skip_dir(dir_name: &OsStr, exclude: &[String]) -> bool {
    let name = dir_name.to_string_lossy();
    if BUILTIN_SKIP_DIRS.iter().any(|s| *s == name.as_ref()) {
        return true;
    }
    // Skip rude database directories
    if name.starts_with(".rude") {
        return true;
    }
    exclude.iter().any(|e| e == name.as_ref())
}

/// Recursively scan `input` for files whose extension passes `ext_filter`,
/// skipping built-in + user-specified directories.
pub fn scan_files(
    input: &Path,
    exclude: &[String],
    ext_filter: impl Fn(&str) -> bool,
) -> Vec<PathBuf> {
    walkdir::WalkDir::new(input)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                !should_skip_dir(e.file_name(), exclude)
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(&ext_filter)
                .unwrap_or(false)
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}
