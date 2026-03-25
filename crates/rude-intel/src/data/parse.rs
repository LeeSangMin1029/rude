#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
pub struct ParsedChunk {
    pub kind: String,
    pub name: String,
    pub file: String,
    pub lines: Option<(usize, usize)>,
    pub signature: Option<String>,
    pub calls: Vec<String>,
    pub call_lines: Vec<u32>,
    pub types: Vec<String>,
    #[serde(default)]
    pub imports: Vec<String>,
    #[serde(default)]
    pub string_args: Vec<(String, String, u32, u8)>,
    #[serde(default)]
    pub param_flows: Vec<(String, u8, String, u8, u32)>,
    #[serde(default)]
    pub param_types: Vec<(String, String)>,
    #[serde(default)]
    pub field_types: Vec<(String, String)>,
    #[serde(default)]
    pub local_types: Vec<(String, String)>,
    #[serde(default)]
    pub let_call_bindings: Vec<(String, String)>,
    #[serde(default)]
    pub return_type: Option<String>,
    #[serde(default)]
    pub field_accesses: Vec<(String, String)>,
    #[serde(default)]
    pub enum_variants: Vec<String>,
    #[serde(default)]
    pub is_test: bool,
    #[serde(default)]
    pub visibility: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub chunk_index: usize,
    #[serde(default)]
    pub minhash: Option<String>,
}

impl ParsedChunk {
    /// Compute minhash from text and store it in the `minhash` field.
    pub fn compute_minhash(&mut self) {
        let tokens = crate::minhash::code_tokens(&self.text);
        if tokens.len() >= 10 {
            self.minhash = Some(crate::minhash::minhash_to_hex(
                &crate::minhash::minhash_signature(&tokens, crate::minhash::MINHASH_K),
            ));
        }
    }

}

pub fn normalize_path(p: &str) -> String {
    let s = p.replace('\\', "/");
    let s = s.strip_prefix("./").unwrap_or(&s);

    let s = s.strip_prefix("//?/").unwrap_or(s);

    if let Some(root) = PROJECT_ROOT.get() {
        if let Some(rel) = s.strip_prefix(root.as_str()) {
            let rel = rel.strip_prefix('/').unwrap_or(rel);
            if !rel.is_empty() {
                return rel.to_owned();
            }
        }
        // Also try case-insensitive match (Windows drive letters)
        let s_lower = s.to_lowercase();
        let root_lower = root.to_lowercase();
        if let Some(rel) = s_lower.strip_prefix(root_lower.as_str()) {
            let rel = rel.strip_prefix('/').unwrap_or(rel);
            if !rel.is_empty() {
                return s[s.len() - rel.len()..].to_owned();
            }
        }
    }

    s.to_owned()
}

use std::sync::OnceLock;

static PROJECT_ROOT: OnceLock<String> = OnceLock::new();

pub fn set_project_root(root: &std::path::Path) {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut s = canonical.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = s.strip_prefix("//?/") {
        s = stripped.to_owned();
    }
    while s.ends_with('/') {
        s.pop();
    }
    let _ = PROJECT_ROOT.set(s);
}
