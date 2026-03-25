//! SQLite-based storage engine for rude.
//!
//! Replaces the mmap + WAL + file-based payload store from v-hnsw-storage
//! with a single SQLite database. API is kept similar so callers only need
//! to change their import paths.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};

use crate::payload::{Payload, PayloadValue};
use crate::payload_store::PayloadStore;

/// SQLite-backed storage engine.
///
/// The database file lives at `<dir>/store.db`. SQLite WAL mode provides
/// single-writer + multi-reader concurrency automatically.
pub struct StorageEngine {
    conn: Connection,
    dir: PathBuf,
}

impl StorageEngine {
    // ------------------------------------------------------------------
    // Open / create
    // ------------------------------------------------------------------

    /// Open an existing database in read-write mode (no exclusive lock).
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let db_path = dir.join("store.db");

        if !db_path.exists() {
            bail!("database not found: {}", db_path.display());
        }

        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database: {}", db_path.display()))?;

        Self::apply_pragmas(&conn)?;

        Ok(Self { conn, dir })
    }

    /// Open (or create) a database with schema initialization.
    ///
    /// SQLite WAL mode handles write exclusivity automatically.
    pub fn open_exclusive(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create directory: {}", dir.display()))?;

        let db_path = dir.join("store.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database: {}", db_path.display()))?;

        Self::apply_pragmas(&conn)?;
        Self::ensure_schema(&conn)?;

        Ok(Self { conn, dir })
    }

    /// Apply performance-oriented PRAGMAs.
    fn apply_pragmas(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -64000;
             PRAGMA mmap_size = 268435456;
             PRAGMA temp_store = MEMORY;
             PRAGMA busy_timeout = 5000;",
        )
        .context("failed to set pragmas")?;
        Ok(())
    }

    /// Create tables if they don't exist yet.
    fn ensure_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chunks (
                 id        INTEGER PRIMARY KEY,
                 source    TEXT NOT NULL,
                 tags      TEXT NOT NULL DEFAULT '[]',
                 custom    TEXT NOT NULL DEFAULT '{}',
                 created_at       INTEGER NOT NULL DEFAULT 0,
                 source_modified_at INTEGER NOT NULL DEFAULT 0,
                 chunk_index      INTEGER NOT NULL DEFAULT 0,
                 chunk_total      INTEGER NOT NULL DEFAULT 0,
                 text      TEXT NOT NULL DEFAULT ''
             );
             CREATE INDEX IF NOT EXISTS idx_chunks_source ON chunks(source);

             CREATE TABLE IF NOT EXISTS file_index (
                 path          TEXT PRIMARY KEY,
                 mtime         INTEGER NOT NULL,
                 size          INTEGER NOT NULL,
                 chunk_ids     TEXT NOT NULL DEFAULT '[]',
                 content_hash  INTEGER
             );

             CREATE TABLE IF NOT EXISTS config (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )
        .context("failed to create schema")?;
        Ok(())
    }

    /// Acquire a blocking exclusive lock on the database directory.
    // ------------------------------------------------------------------
    // Write operations
    // ------------------------------------------------------------------

    /// Insert a chunk (id, payload, text). No vector argument — vectors
    /// are handled externally in rude.
    pub fn insert(&mut self, id: u64, payload: &Payload, text: &str) -> Result<()> {
        let tags_json = serde_json::to_string(&payload.tags)
            .context("failed to serialize tags")?;
        let custom_json = serde_json::to_string(&payload.custom)
            .context("failed to serialize custom")?;

        self.conn
            .execute(
                "INSERT OR REPLACE INTO chunks
                     (id, source, tags, custom, created_at, source_modified_at,
                      chunk_index, chunk_total, text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id as i64,
                    payload.source,
                    tags_json,
                    custom_json,
                    payload.created_at as i64,
                    payload.source_modified_at as i64,
                    payload.chunk_index,
                    payload.chunk_total,
                    text,
                ],
            )
            .context("failed to insert chunk")?;

        Ok(())
    }

    /// Insert a batch of chunks inside a single transaction.
    pub fn insert_batch(&mut self, batch: &[(u64, Payload, &str)]) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }

        let tx = self.conn.transaction().context("failed to begin transaction")?;

        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO chunks
                         (id, source, tags, custom, created_at, source_modified_at,
                          chunk_index, chunk_total, text)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .context("failed to prepare insert")?;

            for (id, payload, text) in batch {
                let tags_json = serde_json::to_string(&payload.tags)
                    .context("failed to serialize tags")?;
                let custom_json = serde_json::to_string(&payload.custom)
                    .context("failed to serialize custom")?;

                stmt.execute(params![
                    *id as i64,
                    payload.source,
                    tags_json,
                    custom_json,
                    payload.created_at as i64,
                    payload.source_modified_at as i64,
                    payload.chunk_index,
                    payload.chunk_total,
                    *text,
                ])
                .context("failed to insert chunk")?;
            }
        }

        tx.commit().context("failed to commit transaction")?;
        Ok(())
    }

    /// Remove a point and all associated data.
    pub fn remove(&mut self, id: u64) -> Result<()> {
        self.conn
            .execute("DELETE FROM chunks WHERE id = ?1", params![id as i64])
            .context("failed to remove chunk")?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Read operations
    // ------------------------------------------------------------------

    /// Get payload for a point.
    pub fn get_payload(&self, id: u64) -> Result<Option<Payload>> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT source, tags, custom, created_at, source_modified_at,
                        chunk_index, chunk_total
                 FROM chunks WHERE id = ?1",
            )
            .context("failed to prepare get_payload")?;

        let mut rows = stmt
            .query_map(params![id as i64], |row| {
                Ok(PayloadRow {
                    source: row.get(0)?,
                    tags_json: row.get(1)?,
                    custom_json: row.get(2)?,
                    created_at: row.get(3)?,
                    source_modified_at: row.get(4)?,
                    chunk_index: row.get(5)?,
                    chunk_total: row.get(6)?,
                })
            })
            .context("failed to query payload")?;

        match rows.next() {
            Some(row) => {
                let row = row.context("failed to read payload row")?;
                Ok(Some(Self::row_to_payload(&row)?))
            }
            None => Ok(None),
        }
    }

    /// Get text for a point.
    pub fn get_text(&self, id: u64) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT text FROM chunks WHERE id = ?1")
            .context("failed to prepare get_text")?;

        let mut rows = stmt
            .query_map(params![id as i64], |row| row.get(0))
            .context("failed to query text")?;

        match rows.next() {
            Some(text) => Ok(Some(text.context("failed to read text")?)),
            None => Ok(None),
        }
    }

    /// Iterate all chunks: returns `(id, payload, text)` tuples.
    ///
    /// Used for `load_chunks_from_db` equivalent.
    pub fn iter_all(&self) -> Result<Vec<(u64, Payload, String)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, source, tags, custom, created_at, source_modified_at,
                        chunk_index, chunk_total, text
                 FROM chunks ORDER BY id",
            )
            .context("failed to prepare iter_all")?;

        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                Ok((
                    id as u64,
                    PayloadRow {
                        source: row.get(1)?,
                        tags_json: row.get(2)?,
                        custom_json: row.get(3)?,
                        created_at: row.get(4)?,
                        source_modified_at: row.get(5)?,
                        chunk_index: row.get(6)?,
                        chunk_total: row.get(7)?,
                    },
                    row.get::<_, String>(8)?,
                ))
            })
            .context("failed to query all chunks")?;

        let mut result = Vec::new();
        for row in rows {
            let (id, prow, text) = row.context("failed to read row")?;
            let payload = Self::row_to_payload(&prow)?;
            result.push((id, payload, text));
        }
        Ok(result)
    }

    /// Get all stored point IDs sorted.
    pub fn all_ids(&self) -> Result<Vec<u64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM chunks ORDER BY id")
            .context("failed to prepare all_ids")?;

        let ids: Vec<u64> = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                Ok(id as u64)
            })
            .context("failed to query all ids")?
            .filter_map(|r| r.ok())
            .collect();

        Ok(ids)
    }

    // ------------------------------------------------------------------
    // Checkpoint / maintenance
    // ------------------------------------------------------------------

    /// WAL checkpoint — forces SQLite to merge the WAL into the main DB file.
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .context("failed to checkpoint WAL")?;
        Ok(())
    }

    /// Database directory path.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Access the raw connection for advanced queries.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Return self as a `PayloadStore` reference.
    ///
    /// Mirrors the v-hnsw-storage `payload_store()` API so callers can
    /// pass `engine.payload_store()` to generic code.
    pub fn payload_store(&self) -> &dyn PayloadStore {
        self
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn row_to_payload(row: &PayloadRow) -> Result<Payload> {
        let tags: Vec<String> =
            serde_json::from_str(&row.tags_json).context("failed to parse tags JSON")?;
        let custom: std::collections::HashMap<String, PayloadValue> =
            serde_json::from_str(&row.custom_json).context("failed to parse custom JSON")?;

        Ok(Payload {
            source: row.source.clone(),
            tags,
            created_at: row.created_at as u64,
            source_modified_at: row.source_modified_at as u64,
            chunk_index: row.chunk_index,
            chunk_total: row.chunk_total,
            custom,
        })
    }
}

// StorageEngine is not Send+Sync (rusqlite::Connection is !Send),
// so we provide a blanket impl only where the trait bound is met.
// For single-threaded use, callers can use StorageEngine methods directly.
// For PayloadStore trait usage across threads, wrap in a Mutex.

impl PayloadStore for StorageEngine {
    fn get_payload(&self, id: u64) -> Result<Option<Payload>> {
        self.get_payload(id)
    }

    fn get_text(&self, id: u64) -> Result<Option<String>> {
        self.get_text(id)
    }
}

/// Intermediate struct for reading payload columns from SQLite.
struct PayloadRow {
    source: String,
    tags_json: String,
    custom_json: String,
    created_at: i64,
    source_modified_at: i64,
    chunk_index: u32,
    chunk_total: u32,
}
