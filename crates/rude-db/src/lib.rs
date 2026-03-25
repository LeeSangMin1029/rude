mod db;
mod db_config;
pub mod file_index;
pub mod file_utils;
pub mod interrupt;
mod payload;
pub use db::StorageEngine;
pub use db_config::DbConfig;
pub use file_index::{
    FileIndex, FileMetadata, get_file_size, load_file_index,
    save_file_index,
};
pub use file_utils::{
    content_hash, content_hash_bytes, generate_id, get_file_mtime, home_dir, is_code_ext,
    lang_for_ext, normalize_source, scan_files, should_skip_dir,
    strip_unc_prefix, strip_unc_prefix_path,
};
pub use payload::{Payload, PayloadValue};
pub use interrupt::is_interrupted;

/// Storage configuration for creating a new database.
/// Kept for API compatibility — sqlite handles capacity internally.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    pub dim: usize,
    pub initial_capacity: usize,
    pub checkpoint_threshold: usize,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self { dim: 1, initial_capacity: 10_000, checkpoint_threshold: 50_000 }
    }
}
