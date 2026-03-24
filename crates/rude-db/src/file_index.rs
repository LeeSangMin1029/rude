//! File metadata tracking for incremental updates.
//!
//! Maintains a SQLite table mapping source files to their modification time,
//! size, and associated chunk IDs for efficient incremental processing.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Metadata for a single source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    /// Source file path.
    pub path: String,
    /// Last modification time (Unix timestamp).
    pub mtime: u64,
    /// File size in bytes.
    pub size: u64,
    /// IDs of chunks generated from this file.
    pub chunk_ids: Vec<u64>,
    /// MD5-based content hash (truncated to u64) for change detection.
    /// None for entries created before this field was added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<u64>,
}

/// File index structure stored as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIndex {
    /// Index format version.
    pub version: u32,
    /// Map from file path to metadata.
    pub files: HashMap<String, FileMetadata>,
}

impl FileIndex {
    /// Current index version.
    pub const VERSION: u32 = 1;

    /// Create a new empty file index.
    pub fn new() -> Self {
        Self {
            version: Self::VERSION,
            files: HashMap::new(),
        }
    }

    /// Add or update file metadata.
    pub fn update_file(&mut self, path: String, mtime: u64, size: u64, chunk_ids: Vec<u64>) {
        self.files.insert(
            path.clone(),
            FileMetadata {
                path,
                mtime,
                size,
                chunk_ids,
                content_hash: None,
            },
        );
    }

    /// Add or update file metadata with content hash.
    pub fn update_file_with_hash(
        &mut self,
        path: String,
        mtime: u64,
        size: u64,
        chunk_ids: Vec<u64>,
        content_hash: u64,
    ) {
        self.files.insert(
            path.clone(),
            FileMetadata {
                path,
                mtime,
                size,
                chunk_ids,
                content_hash: Some(content_hash),
            },
        );
    }

    /// Get metadata for a file.
    pub fn get_file(&self, path: &str) -> Option<&FileMetadata> {
        self.files.get(path)
    }

    /// Check if a file has been modified since last index.
    pub fn is_modified(&self, path: &str, mtime: u64, size: u64) -> bool {
        match self.files.get(path) {
            Some(meta) => meta.mtime != mtime || meta.size != size,
            None => true, // New file
        }
    }
}

impl Default for FileIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Load file index from database directory.
pub fn load_file_index(db_path: &Path) -> Result<FileIndex> {
    let index_path = db_path.join("file_index.json");

    if !index_path.exists() {
        return Ok(FileIndex::new());
    }

    let data = std::fs::read_to_string(&index_path)
        .with_context(|| format!("Failed to read file index at {}", index_path.display()))?;

    let index: FileIndex = serde_json::from_str(&data)
        .with_context(|| format!("Failed to parse file index at {}", index_path.display()))?;

    Ok(index)
}

/// Save file index to database directory.
pub fn save_file_index(db_path: &Path, index: &FileIndex) -> Result<()> {
    let index_path = db_path.join("file_index.json");

    let data = serde_json::to_string_pretty(index)
        .with_context(|| "Failed to serialize file index")?;

    std::fs::write(&index_path, data)
        .with_context(|| format!("Failed to write file index to {}", index_path.display()))?;

    Ok(())
}

/// Get file size in bytes.
pub fn get_file_size(path: &Path) -> Result<u64> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("Failed to read metadata for {}", path.display()))?;

    Ok(metadata.len())
}
