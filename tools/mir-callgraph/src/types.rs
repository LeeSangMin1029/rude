use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct CallEdge {
    pub caller: String,
    pub caller_file: String,
    pub caller_kind: String,
    pub callee: String,
    pub callee_file: String,
    pub callee_start_line: usize,
    pub line: usize,
    pub is_local: bool,
}

#[derive(Serialize)]
pub struct MirChunk {
    pub name: String,
    pub file: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: Option<String>,
    pub visibility: String,
    pub is_test: bool,
    pub body: String,
    #[serde(default)]
    pub calls: String,
    #[serde(default)]
    pub type_refs: String,
    #[serde(default)]
    pub field_accesses: String,
}

#[derive(Serialize, Deserialize)]
pub struct RustcArgs {
    pub args: Vec<String>,
    pub crate_name: String,
    pub sysroot: String,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

impl RustcArgs {
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| format!("read error {path}: {e}"))?;
        serde_json::from_str(&content).map_err(|e| format!("parse error {path}: {e}"))
    }
}

#[derive(Serialize)]
pub struct UseItem {
    pub file: String,
    pub line: usize,
    pub source: String,
    pub resolved: String,
}

#[derive(Serialize)]
pub struct UseDep {
    pub fn_name: String,
    pub fn_file: String,
    pub use_file: String,
    pub use_line: usize,
}

pub fn env_config() -> (bool, Option<String>) {
    (std::env::var("MIR_CALLGRAPH_JSON").is_ok(), std::env::var("MIR_CALLGRAPH_DB").ok())
}
