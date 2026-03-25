use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::StorageEngine;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbConfig {
    pub version: u32,
    pub korean: bool,
    /// Embedding model used (for search auto-detection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<String>,
    pub code: bool,
    /// Original input path used during `add` (for `update` default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_path: Option<String>,
    /// Whether vectors have been embedded (false = text-only, zero vectors).
    #[serde(default = "default_true")]
    pub embedded: bool,
}

fn default_true() -> bool {
    true
}

impl Default for DbConfig {
    fn default() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            korean: false,
            embed_model: None,
            code: false,
            input_path: None,
            embedded: true,
        }
    }
}

impl DbConfig {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn load(path: &Path) -> Result<Self> {
        let store_db = path.join("store.db");

        if !store_db.exists() {
            // Fall back to legacy JSON if store.db hasn't been created yet
            return Self::load_legacy_json(path);
        }

        let engine = StorageEngine::open(path)
            .with_context(|| "failed to open store.db for config")?;
        Self::load_from_engine(&engine)
    }

    pub fn load_from_engine(engine: &StorageEngine) -> Result<Self> {
        let conn = engine.connection();

        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='config'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);

        if !table_exists {
            return Self::load_legacy_json(engine.dir());
        }

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM config", [], |row| row.get(0))
            .unwrap_or(0);

        if count == 0 {
            return Self::load_legacy_json(engine.dir());
        }

        let get = |key: &str| -> Result<Option<String>> {
            let mut stmt = conn
                .prepare_cached("SELECT value FROM config WHERE key = ?1")
                .context("failed to prepare config SELECT")?;
            let mut rows = stmt
                .query_map(params![key], |row| row.get::<_, String>(0))
                .context("failed to query config")?;
            match rows.next() {
                Some(v) => Ok(Some(v.context("failed to read config value")?)),
                None => Ok(None),
            }
        };

        let version = get("version")?
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(Self::CURRENT_VERSION);
        let korean = get("korean")?
            .map(|v| v == "true")
            .unwrap_or(false);
        let embed_model = get("embed_model")?
            .filter(|v| !v.is_empty());
        let code = get("code")?
            .map(|v| v == "true")
            .unwrap_or(false);
        let input_path = get("input_path")?
            .filter(|v| !v.is_empty());
        let embedded = get("embedded")?
            .map(|v| v != "false")
            .unwrap_or(true);

        Ok(Self {
            version,
            korean,
            embed_model,
            code,
            input_path,
            embedded,
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let engine = StorageEngine::open(path)
            .with_context(|| "failed to open store.db for saving config")?;
        self.save_to_engine(&engine)
    }

    pub fn save_to_engine(&self, engine: &StorageEngine) -> Result<()> {
        let conn = engine.connection();

        let mut stmt = conn
            .prepare_cached(
                "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
            )
            .context("failed to prepare config INSERT")?;

        stmt.execute(params!["version", self.version.to_string()])
            .context("failed to save config version")?;
        stmt.execute(params!["korean", self.korean.to_string()])
            .context("failed to save config korean")?;
        stmt.execute(params![
            "embed_model",
            self.embed_model.as_deref().unwrap_or("")
        ])
        .context("failed to save config embed_model")?;
        stmt.execute(params!["code", self.code.to_string()])
            .context("failed to save config code")?;
        stmt.execute(params![
            "input_path",
            self.input_path.as_deref().unwrap_or("")
        ])
        .context("failed to save config input_path")?;
        stmt.execute(params!["embedded", self.embedded.to_string()])
            .context("failed to save config embedded")?;

        Ok(())
    }

    fn load_legacy_json(path: &Path) -> Result<Self> {
        let config_path = path.join("config.json");
        let data = std::fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read config: {}", config_path.display()))?;
        let config: Self = serde_json::from_str(&data)
            .with_context(|| "failed to parse config.json")?;
        Ok(config)
    }
}
