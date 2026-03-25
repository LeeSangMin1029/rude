use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};

use crate::payload::{Payload, PayloadValue};
use crate::payload_store::PayloadStore;

pub struct StorageEngine {
    conn: Connection,
}

impl StorageEngine {
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let db_path = dir.join("store.db");
        if !db_path.exists() {
            bail!("database not found: {}", db_path.display());
        }
        Self::open_impl(dir, false)
    }

    pub fn open_exclusive(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create directory: {}", dir.display()))?;
        Self::open_impl(dir, true)
    }

    fn open_impl(dir: PathBuf, init_schema: bool) -> Result<Self> {
        let db_path = dir.join("store.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database: {}", db_path.display()))?;
        Self::apply_pragmas(&conn)?;
        if init_schema {
            Self::ensure_schema(&conn)?;
        }
        Ok(Self { conn })
    }

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
             CREATE TABLE IF NOT EXISTS kv_cache (
                 key   TEXT PRIMARY KEY,
                 value BLOB NOT NULL
             );",
        )
        .context("failed to create schema")?;
        Ok(())
    }

    pub fn insert(&mut self, id: u64, payload: &Payload, text: &str) -> Result<()> {
        self.insert_batch(&[(id, payload.clone(), text)])
    }

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

    pub fn remove(&mut self, id: u64) -> Result<()> {
        self.conn
            .execute("DELETE FROM chunks WHERE id = ?1", params![id as i64])
            .context("failed to remove chunk")?;
        Ok(())
    }

    pub fn get_payload(&self, id: u64) -> Result<Option<Payload>> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT source, tags, custom, created_at, source_modified_at,
                        chunk_index, chunk_total
                 FROM chunks WHERE id = ?1",
            )
            .context("failed to prepare get_payload")?;

        let row = Self::query_first(&mut stmt, params![id as i64], |row| {
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

        row.map(|r| Self::row_to_payload(&r)).transpose()
    }

    pub fn get_text(&self, id: u64) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT text FROM chunks WHERE id = ?1")
            .context("failed to prepare get_text")?;

        Self::query_first(&mut stmt, params![id as i64], |row| row.get(0))
            .context("failed to query text")
    }

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

    /// WAL checkpoint — forces SQLite to merge the WAL into the main DB file.
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .context("failed to checkpoint WAL")?;
        Ok(())
    }

    pub fn get_cache(&self, key: &str) -> Result<Option<Vec<u8>>> {
        // Ensure kv_cache table exists (for databases created before this schema addition)
        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS kv_cache (key TEXT PRIMARY KEY, value BLOB NOT NULL)",
                [],
            )
            .ok();

        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM kv_cache WHERE key = ?1")
            .context("failed to prepare kv_cache SELECT")?;
        let mut rows = stmt
            .query_map(params![key], |row| row.get::<_, Vec<u8>>(0))
            .context("failed to query kv_cache")?;
        match rows.next() {
            Some(v) => Ok(Some(v.context("failed to read kv_cache value")?)),
            None => Ok(None),
        }
    }

    pub fn set_cache(&self, key: &str, data: &[u8]) -> Result<()> {
        // Ensure kv_cache table exists
        self.conn
            .execute(
                "CREATE TABLE IF NOT EXISTS kv_cache (key TEXT PRIMARY KEY, value BLOB NOT NULL)",
                [],
            )
            .ok();

        self.conn
            .execute(
                "INSERT OR REPLACE INTO kv_cache (key, value) VALUES (?1, ?2)",
                params![key, data],
            )
            .context("failed to set kv_cache value")?;
        Ok(())
    }

    pub fn payload_store(&self) -> &dyn PayloadStore {
        self
    }

    fn query_first<T, P, F>(
        stmt: &mut rusqlite::CachedStatement<'_>,
        params: P,
        f: F,
    ) -> Result<Option<T>>
    where
        P: rusqlite::Params,
        F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        let mut rows = stmt.query_map(params, f)?;
        match rows.next() {
            Some(val) => Ok(Some(val?)),
            None => Ok(None),
        }
    }

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

// rusqlite::Connection is !Send, so for PayloadStore trait usage across threads, wrap in a Mutex.
impl PayloadStore for StorageEngine {
    fn get_payload(&self, id: u64) -> Result<Option<Payload>> {
        self.get_payload(id)
    }

    fn get_text(&self, id: u64) -> Result<Option<String>> {
        self.get_text(id)
    }
}

struct PayloadRow {
    source: String,
    tags_json: String,
    custom_json: String,
    created_at: i64,
    source_modified_at: i64,
    chunk_index: u32,
    chunk_total: u32,
}
