//! Database configuration (shared between doc and code).

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Database metadata stored in config.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbConfig {
    /// Database format version.
    pub version: u32,
    /// Whether Korean tokenizer is enabled.
    pub korean: bool,
    /// Embedding model used (for search auto-detection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<String>,
    /// Whether this is a code database (uses CodeTokenizer for BM25).
    #[serde(default)]
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

    /// Load config from database directory (reads `config.json`).
    pub fn load(path: &Path) -> Result<Self> {
        let config_path = path.join("config.json");
        let data = std::fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read config: {}", config_path.display()))?;
        let config: Self = serde_json::from_str(&data)
            .with_context(|| "failed to parse config.json")?;
        Ok(config)
    }

    /// Save config to database directory (writes `config.json`).
    pub fn save(&self, path: &Path) -> Result<()> {
        let config_path = path.join("config.json");
        let data = serde_json::to_string_pretty(self)
            .with_context(|| "failed to serialize config")?;
        std::fs::write(&config_path, data)
            .with_context(|| format!("failed to write config: {}", config_path.display()))?;
        Ok(())
    }
}
