
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct MirEdgeMap {
    pub by_location: HashMap<(String, usize), Vec<String>>,
    pub by_caller: HashMap<String, Vec<CalleeInfo>>,
    pub caller_files: HashMap<String, Vec<String>>,
    pub total: usize,
    pub caller_crate: HashMap<String, String>,
    pub crate_to_callers: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct CalleeInfo {
    pub name: String,
    pub file: String,
    pub start_line: usize,
    pub call_line: usize,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct MirChunk {
    pub name: String,
    pub file: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub is_test: bool,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub calls: String,
    #[serde(default)]
    pub type_refs: String,
    #[serde(default)]
    pub crate_name: String,
    #[serde(default)]
    pub field_accesses: String,
}

impl MirChunk {
    pub fn to_parsed(&self) -> crate::parse::ParsedChunk {
        let kind = match self.kind.as_str() {
            "fn" | "method" => "function", other => other,
        }.to_owned();
        let (calls, call_lines) = parse_calls_field(&self.calls);
        let types = if self.type_refs.is_empty() {
            Vec::new()
        } else {
            self.type_refs.split(", ").map(|s| s.to_owned()).collect()
        };
        let field_accesses = if self.field_accesses.is_empty() {
            Vec::new()
        } else {
            self.field_accesses.split(", ").filter_map(|entry| {
                let dot = entry.find('.')?;
                Some((entry[..dot].to_owned(), entry[dot + 1..].to_owned()))
            }).collect()
        };
        let mut chunk = crate::parse::ParsedChunk {
            kind, name: self.name.clone(), file: self.file.clone(),
            lines: Some((self.start_line, self.end_line)),
            signature: self.signature.clone(),
            calls, call_lines, types,
            visibility: self.visibility.clone().unwrap_or_default(),
            text: self.body.clone(),
            is_test: self.is_test,
            crate_name: self.crate_name.clone(),
            field_accesses,
            ..Default::default()
        };
        chunk.compute_minhash();
        chunk
    }
}

impl MirEdgeMap {
    pub fn crate_names(&self) -> std::collections::HashSet<&str> {
        self.caller_crate.values().map(String::as_str).collect()
    }

    pub fn callers_for_crate<'a>(&'a self, crate_name: &str) -> Vec<&'a str> {
        self.crate_to_callers.get(crate_name)
            .map(|callers| callers.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

pub fn parse_calls_field(calls_str: &str) -> (Vec<String>, Vec<u32>) {
    if calls_str.is_empty() {
        return (Vec::new(), Vec::new());
    }
    calls_str
        .split(", ")
        .map(|token| {
            if let Some(at) = token.rfind('@') {
                let name = token[..at].to_owned();
                let line: u32 = token[at + 1..].parse().unwrap_or(0);
                (name, line)
            } else {
                (token.to_owned(), 0u32)
            }
        })
        .unzip()
}

