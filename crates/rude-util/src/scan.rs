use std::ffi::OsStr;
use std::path::{Path, PathBuf};

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

pub fn is_code_ext(ext: &str) -> bool {
    lang_for_ext(ext) != "other"
}
pub fn is_supported_code_file(file: &str) -> bool {
    let f = file.replace('\\', "/");
    let ext = f.rsplit('.').next().unwrap_or("");
    is_code_ext(ext)
}

pub fn get_file_mtime(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

pub fn get_file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

const BUILTIN_SKIP_DIRS: &[&str] = &[
    "target", "node_modules", ".git", ".swarm", "__pycache__", ".venv",
    "dist", "vendor", ".tox", ".mypy_cache", ".pytest_cache", ".claude",
    "build", "mutants.out",
];

pub fn should_skip_dir(dir_name: &OsStr, exclude: &[String]) -> bool {
    let name = dir_name.to_string_lossy();
    if BUILTIN_SKIP_DIRS.iter().any(|s| *s == name.as_ref()) {
        return true;
    }
    if name.starts_with(".rude") {
        return true;
    }
    exclude.iter().any(|e| e == name.as_ref())
}

pub fn scan_files(
    input: &Path,
    exclude: &[String],
    ext_filter: impl Fn(&str) -> bool,
) -> Vec<PathBuf> {
    walkdir::WalkDir::new(input)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() { !should_skip_dir(e.file_name(), exclude) } else { true }
        })
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().extension().and_then(|ext| ext.to_str()).map(&ext_filter).unwrap_or(false)
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}
