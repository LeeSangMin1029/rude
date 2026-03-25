//! rude — Code intelligence library.
//!
//! Exposes the `add` command for in-process reindexing by the daemon.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub mod commands;

static DB_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Set the global DB path (call once at startup).
pub fn set_db(path: PathBuf) {
    DB_PATH.set(path).expect("DB path already set");
}

/// Get the global DB path.
pub fn db() -> &'static Path {
    DB_PATH.get().expect("DB path not set — call set_db() first")
}
