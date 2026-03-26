use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub mod commands;

static DB_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn set_db(path: PathBuf) {
    let abs = path.canonicalize().unwrap_or(std::env::current_dir().unwrap_or_default().join(&path));
    let clean = abs.to_string_lossy().strip_prefix(r"\\?\").map(PathBuf::from).unwrap_or(abs);
    DB_PATH.set(clean).expect("DB path already set");
}

pub fn db() -> &'static Path {
    DB_PATH.get().expect("DB path not set — call set_db() first")
}
