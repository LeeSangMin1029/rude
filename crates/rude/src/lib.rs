use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub mod commands;
pub mod config;

static DB_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn set_db(path: PathBuf) {
    DB_PATH.set(rude_util::safe_canonicalize(&path)).expect("DB path already set");
}

pub fn db() -> &'static Path {
    DB_PATH.get().expect("DB path not set — call set_db() first")
}

pub struct WriteLock {
    _file: std::fs::File,
    path: PathBuf,
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn acquire_write_lock() -> anyhow::Result<WriteLock> {
    use fs2::FileExt;
    let path = db().join(".write.lock");
    let file = std::fs::File::create(&path)?;
    file.lock_exclusive()?;
    Ok(WriteLock { _file: file, path })
}
