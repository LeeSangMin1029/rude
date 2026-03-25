use anyhow::{Context, Result};
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

    pub fn load(engine: &StorageEngine) -> Result<Self> {
        match engine.get_cache("config")? {
            Some(blob) => {
                let config: Self = serde_json::from_slice(&blob)
                    .context("failed to parse config from kv_cache")?;
                Ok(config)
            }
            None => Ok(Self::default()),
        }
    }

    pub fn save(&self, engine: &StorageEngine) -> Result<()> {
        let blob = serde_json::to_vec(self).context("failed to serialize config")?;
        engine.set_cache("config", &blob)?;
        Ok(())
    }
}
