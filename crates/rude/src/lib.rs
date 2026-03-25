use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub mod commands;

static DB_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn set_db(path: PathBuf) {
    DB_PATH.set(path).expect("DB path already set");
}

pub fn db() -> &'static Path {
    DB_PATH.get().expect("DB path not set — call set_db() first")
}
