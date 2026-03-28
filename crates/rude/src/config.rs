use std::path::Path;
use std::sync::OnceLock;
use serde::Deserialize;

static CONFIG: OnceLock<RudeConfig> = OnceLock::new();

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct RudeConfig {
    pub split: SplitConfig,
    pub cluster: ClusterConfig,
    pub watch: WatchConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct SplitConfig {
    pub min_lines: usize,
}
impl Default for SplitConfig {
    fn default() -> Self { Self { min_lines: 300 } }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct ClusterConfig {
    pub min_lines: usize,
}
impl Default for ClusterConfig {
    fn default() -> Self { Self { min_lines: 50 } }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct WatchConfig {
    pub ignored_dirs: Vec<String>,
}
impl Default for WatchConfig {
    fn default() -> Self {
        Self { ignored_dirs: vec![
            ".git".into(), "target".into(), "node_modules".into(), "__pycache__".into(),
        ]}
    }
}

pub fn load(db_path: &Path) {
    let config_path = db_path.join("config.toml");
    let config = if config_path.exists() {
        match std::fs::read_to_string(&config_path) {
            Ok(content) => toml::from_str(&content).unwrap_or_else(|e| {
                eprintln!("warning: invalid config.toml: {e}");
                RudeConfig::default()
            }),
            Err(_) => RudeConfig::default(),
        }
    } else {
        RudeConfig::default()
    };
    CONFIG.set(config).ok();
}

pub fn get() -> &'static RudeConfig {
    CONFIG.get_or_init(RudeConfig::default)
}
