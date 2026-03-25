use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::StorageEngine;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    pub path: String,
    pub mtime: u64,
    pub size: u64,
    pub chunk_ids: Vec<u64>,
    /// MD5-based content hash (truncated to u64) for change detection.
    /// None for entries created before this field was added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIndex {
    pub version: u32,
    pub files: HashMap<String, FileMetadata>,
}

impl FileIndex {
    pub const VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            version: Self::VERSION,
            files: HashMap::new(),
        }
    }

    pub fn update_file(
        &mut self,
        path: String,
        mtime: u64,
        size: u64,
        chunk_ids: Vec<u64>,
        content_hash: Option<u64>,
    ) {
        self.files.insert(
            path.clone(),
            FileMetadata {
                path,
                mtime,
                size,
                chunk_ids,
                content_hash,
            },
        );
    }

    pub fn get_file(&self, path: &str) -> Option<&FileMetadata> {
        self.files.get(path)
    }
}

impl Default for FileIndex {
    fn default() -> Self {
        Self::new()
    }
}

pub fn load_file_index(engine: &StorageEngine) -> Result<FileIndex> {
    match engine.get_cache("file_index")? {
        Some(blob) => {
            let index: FileIndex = serde_json::from_slice(&blob)
                .context("failed to parse file_index from kv_cache")?;
            Ok(index)
        }
        None => Ok(FileIndex::new()),
    }
}

pub fn save_file_index(engine: &StorageEngine, index: &FileIndex) -> Result<()> {
    let blob = serde_json::to_vec(index).context("failed to serialize file_index")?;
    engine.set_cache("file_index", &blob)?;
    Ok(())
}

pub fn get_file_size(path: &Path) -> Result<u64> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("Failed to read metadata for {}", path.display()))?;

    Ok(metadata.len())
}
