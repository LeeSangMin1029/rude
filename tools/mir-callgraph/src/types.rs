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
}

#[derive(Serialize, Deserialize)]
pub struct RustcArgs {
    pub args: Vec<String>,
    pub crate_name: String,
    pub sysroot: String,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}
