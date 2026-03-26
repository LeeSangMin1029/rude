use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};

pub struct StorageEngine {
    conn: Connection,
}

impl StorageEngine {
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let db_path = dir.as_ref().join("store.db");
        if !db_path.exists() { bail!("database not found: {}", db_path.display()); }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database: {}", db_path.display()))?;
        Self::apply_pragmas(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_exclusive(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create directory: {}", dir.display()))?;
        let db_path = dir.join("store.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database: {}", db_path.display()))?;
        Self::apply_pragmas(&conn)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS kv_cache (key TEXT PRIMARY KEY, value BLOB NOT NULL);",
        ).context("failed to create schema")?;
        Ok(Self { conn })
    }

    fn apply_pragmas(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-64000;
             PRAGMA mmap_size=268435456; PRAGMA temp_store=MEMORY; PRAGMA busy_timeout=5000;",
        ).context("failed to set pragmas")
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").context("checkpoint failed")
    }

    pub fn get_cache(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS kv_cache (key TEXT PRIMARY KEY, value BLOB NOT NULL)", [],
        ).ok();
        let mut stmt = self.conn.prepare_cached("SELECT value FROM kv_cache WHERE key = ?1")?;
        let mut rows = stmt.query_map(params![key], |row| row.get::<_, Vec<u8>>(0))?;
        match rows.next() {
            Some(v) => Ok(Some(v?)),
            None => Ok(None),
        }
    }

    pub fn set_cache(&self, key: &str, data: &[u8]) -> Result<()> {
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS kv_cache (key TEXT PRIMARY KEY, value BLOB NOT NULL)", [],
        ).ok();
        self.conn.execute(
            "INSERT OR REPLACE INTO kv_cache (key, value) VALUES (?1, ?2)", params![key, data],
        )?;
        Ok(())
    }
}
