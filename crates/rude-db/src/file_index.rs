//! File metadata tracking for incremental updates.
//!
//! Maintains a SQLite table mapping source files to their modification time,
//! size, and associated chunk IDs for efficient incremental processing.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::StorageEngine;

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

/// File index structure (in-memory cache backed by SQLite).
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

}

impl Default for FileIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Load file index from SQLite `store.db` in the given directory.
///
/// Falls back to reading legacy `file_index.json` if the sqlite table is empty
/// but the JSON file exists (one-time migration).
pub fn load_file_index(db_path: &Path) -> Result<FileIndex> {
    let store_db = db_path.join("store.db");

    if !store_db.exists() {
        // No store.db yet — check legacy JSON
        let json_path = db_path.join("file_index.json");
        if json_path.exists() {
            let data = std::fs::read_to_string(&json_path)
                .with_context(|| format!("Failed to read file index at {}", json_path.display()))?;
            let index: FileIndex = serde_json::from_str(&data)
                .with_context(|| format!("Failed to parse file index at {}", json_path.display()))?;
            return Ok(index);
        }
        return Ok(FileIndex::new());
    }

    let engine = StorageEngine::open(db_path)
        .with_context(|| "failed to open store.db for file_index")?;
    load_file_index_from_engine(&engine)
}

/// Load file index from an already-open `StorageEngine`.
pub fn load_file_index_from_engine(engine: &StorageEngine) -> Result<FileIndex> {
    let conn = engine.connection();

    // Check if file_index table exists
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='file_index'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    if !table_exists {
        // Try legacy JSON migration
        let json_path = engine.dir().join("file_index.json");
        if json_path.exists() {
            let data = std::fs::read_to_string(&json_path)
                .with_context(|| format!("Failed to read file index at {}", json_path.display()))?;
            let index: FileIndex = serde_json::from_str(&data)
                .with_context(|| format!("Failed to parse file index at {}", json_path.display()))?;
            return Ok(index);
        }
        return Ok(FileIndex::new());
    }

    let mut stmt = conn
        .prepare("SELECT path, mtime, size, chunk_ids, content_hash FROM file_index")
        .context("failed to prepare file_index SELECT")?;

    let rows = stmt
        .query_map([], |row| {
            let path: String = row.get(0)?;
            let mtime: i64 = row.get(1)?;
            let size: i64 = row.get(2)?;
            let chunk_ids_json: String = row.get(3)?;
            let content_hash: Option<i64> = row.get(4)?;
            Ok((path, mtime, size, chunk_ids_json, content_hash))
        })
        .context("failed to query file_index")?;

    let mut files = HashMap::new();
    for row in rows {
        let (path, mtime, size, chunk_ids_json, content_hash) =
            row.context("failed to read file_index row")?;
        let chunk_ids: Vec<u64> = serde_json::from_str(&chunk_ids_json)
            .with_context(|| format!("failed to parse chunk_ids for {path}"))?;
        files.insert(
            path.clone(),
            FileMetadata {
                path,
                mtime: mtime as u64,
                size: size as u64,
                chunk_ids,
                content_hash: content_hash.map(|h| h as u64),
            },
        );
    }

    Ok(FileIndex {
        version: FileIndex::VERSION,
        files,
    })
}

/// Save file index to SQLite `store.db` in the given directory.
pub fn save_file_index(db_path: &Path, index: &FileIndex) -> Result<()> {
    let engine = StorageEngine::open(db_path)
        .with_context(|| "failed to open store.db for saving file_index")?;
    save_file_index_to_engine(&engine, index)
}

/// Save file index to an already-open `StorageEngine`.
pub fn save_file_index_to_engine(engine: &StorageEngine, index: &FileIndex) -> Result<()> {
    let conn = engine.connection();

    conn.execute("DELETE FROM file_index", [])
        .context("failed to clear file_index table")?;

    let mut stmt = conn
        .prepare_cached(
            "INSERT INTO file_index (path, mtime, size, chunk_ids, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .context("failed to prepare file_index INSERT")?;

    for meta in index.files.values() {
        let chunk_ids_json =
            serde_json::to_string(&meta.chunk_ids).context("failed to serialize chunk_ids")?;
        stmt.execute(params![
            meta.path,
            meta.mtime as i64,
            meta.size as i64,
            chunk_ids_json,
            meta.content_hash.map(|h| h as i64),
        ])
        .context("failed to insert file_index row")?;
    }

    Ok(())
}

/// Get file size in bytes.
pub fn get_file_size(path: &Path) -> Result<u64> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("Failed to read metadata for {}", path.display()))?;

    Ok(metadata.len())
}
