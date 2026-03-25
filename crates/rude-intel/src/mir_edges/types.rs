//! Core types for MIR-based call edge extraction.

use std::collections::HashMap;

/// Collection of MIR edges indexed for fast lookup by caller.
#[derive(Debug, Default)]
pub struct MirEdgeMap {
    /// (caller_file_normalized, line) → Vec<callee_name>
    pub by_location: HashMap<(String, usize), Vec<String>>,
    /// caller_name → Vec<(callee_name, callee_file, callee_start_line, call_line)>
    pub by_caller: HashMap<String, Vec<CalleeInfo>>,
    /// Total edge count
    pub total: usize,
    /// caller_name → crate_name (tracks which crate each caller belongs to)
    pub caller_crate: HashMap<String, String>,
    /// Reverse index: crate_name → Vec<caller_name> (for O(1) crate lookup)
    pub crate_to_callers: HashMap<String, Vec<String>>,
}

/// Callee information from a MIR edge.
#[derive(Debug, Clone)]
pub struct CalleeInfo {
    pub name: String,
    pub file: String,
    pub start_line: usize,
    pub call_line: usize,
}

/// A chunk definition extracted from MIR — function/struct/enum with location info.
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
}

impl MirEdgeMap {
    /// Get the set of all unique crate names present in this edge map.
    pub fn crate_names(&self) -> std::collections::HashSet<&str> {
        self.caller_crate.values().map(String::as_str).collect()
    }

    /// Get callers belonging to a specific crate.
    /// Uses a pre-built reverse index for O(1) lookup instead of O(N) scan.
    pub fn callers_for_crate<'a>(&'a self, crate_name: &str) -> Vec<&'a str> {
        self.crate_to_callers.get(crate_name)
            .map(|callers| callers.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

/// Parse `"callee@line, callee@line, ..."` format into `(calls, call_lines)`.
///
/// Each token is `callee_name@line_number`. If the `@line` suffix is absent,
/// line defaults to `0`.
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

/// Convert MirChunks directly to ParsedChunks, skipping text format intermediary.
pub fn mir_chunks_to_parsed(mir_chunks: &[MirChunk]) -> Vec<crate::parse::ParsedChunk> {
    mir_chunks
        .iter()
        .map(|mc| {
            let kind = match mc.kind.as_str() {
                "fn" | "method" => "function".to_string(),
                other => other.to_string(),
            };

            let (calls, call_lines) = parse_calls_field(&mc.calls);

            let types: Vec<String> = if mc.type_refs.is_empty() {
                Vec::new()
            } else {
                mc.type_refs.split(", ").map(|s| s.to_string()).collect()
            };

            crate::parse::ParsedChunk {
                kind,
                name: mc.name.clone(),
                file: mc.file.clone(),
                lines: Some((mc.start_line, mc.end_line)),
                signature: mc.signature.clone(),
                calls,
                call_lines,
                types,
                imports: Vec::new(),
                string_args: Vec::new(),
                param_flows: Vec::new(),
                param_types: Vec::new(),
                field_types: Vec::new(),
                local_types: Vec::new(),
                let_call_bindings: Vec::new(),
                return_type: None,
                field_accesses: Vec::new(),
                enum_variants: Vec::new(),
                is_test: mc.is_test,
            }
        })
        .collect()
}
