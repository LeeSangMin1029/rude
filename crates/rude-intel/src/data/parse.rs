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
}

impl ParsedChunk {
    pub fn from_code_chunk(chunk: &crate::chunk_types::CodeChunk, file: &str, imports: Vec<String>) -> Self {
        let lines = if chunk.start_line > 0 || chunk.end_line > 0 {
            Some((chunk.start_line + 1, chunk.end_line + 1))
        } else {
            None
        };
        Self {
            kind: chunk.kind.as_str().to_owned(),
            name: chunk.name.clone(),
            file: normalize_path(file),
            lines, imports, is_test: chunk.is_test,
            signature: chunk.signature.clone(),
            calls: chunk.calls.clone(),
            call_lines: chunk.call_lines.iter().map(|l| l + 1).collect(),
            types: chunk.type_refs.clone(),
            string_args: chunk.string_args.clone(),
            param_flows: chunk.param_flows.clone(),
            param_types: chunk.param_types.clone(),
            field_types: chunk.field_types.clone(),
            local_types: chunk.local_types.clone(),
            let_call_bindings: chunk.let_call_bindings.clone(),
            return_type: chunk.return_type.clone(),
            field_accesses: chunk.field_accesses.clone(),
            enum_variants: chunk.enum_variants.clone(),
        }
    }
}

fn parse_pairs(s: &str, sep: &str) -> Vec<(String, String)> {
    s.split(", ").filter_map(|tok| {
        let tok = tok.trim();
        let pos = tok.find(sep)?;
        Some((tok[..pos].to_owned(), tok[pos + sep.len()..].to_owned()))
    }).collect()
}

fn parse_colon_pairs(s: &str) -> Vec<(String, String)> { parse_pairs(s, ": ") }
fn parse_eq_pairs(s: &str)    -> Vec<(String, String)> { parse_pairs(s, "=") }
fn parse_dot_pairs(s: &str)   -> Vec<(String, String)> { parse_pairs(s, ".") }

fn parse_call_string_args(s: &str) -> Vec<(String, String, u32, u8)> {
    s.split(", ").filter_map(|tok| {
        let tok = tok.trim();
        let paren = tok.find('(')?;
        let end = tok.rfind(')')?;
        if paren >= end { return None; }
        let callee = tok[..paren].to_owned();
        let value = tok[paren + 1..end].trim_matches('"').to_owned();
        Some((callee, value, 0, 0))
    }).collect()
}

pub fn parse_chunk(text: &str) -> Option<ParsedChunk> {
    let mut lines_iter = text.lines();
    let first = lines_iter.next()?;

    if !first.starts_with('[') {
        return None;
    }
    let bracket_end = first.find(']')?;
    let kind = first[1..bracket_end].to_owned();
    let rest = first[bracket_end + 1..].trim();

    let name = {
        let stripped = rest.strip_prefix("pub(crate) ")
            .or_else(|| rest.strip_prefix("pub "))
            .or_else(|| if kind != "impl" && kind != "trait" { rest.strip_prefix("export ") } else { None })
            .unwrap_or(rest);
        stripped.to_owned()
    };

    let mut file = String::new();
    let mut line_range = None;
    let mut signature = None;
    let mut calls = Vec::new();
    let mut call_lines: Vec<u32> = Vec::new();
    let mut types = Vec::new();
    let mut string_args: Vec<(String, String, u32, u8)> = Vec::new();
    let mut param_flows: Vec<(String, u8, String, u8, u32)> = Vec::new();
    let mut param_types: Vec<(String, String)> = Vec::new();
    let mut field_types: Vec<(String, String)> = Vec::new();
    let mut local_types: Vec<(String, String)> = Vec::new();
    let mut let_call_bindings: Vec<(String, String)> = Vec::new();
    let mut field_accesses: Vec<(String, String)> = Vec::new();
    let mut return_type: Option<String> = None;
    let mut enum_variants: Vec<String> = Vec::new();

    for line in lines_iter {
        if let Some(f) = line.strip_prefix("File: ") {
            let f = f.trim();
            if let Some(colon) = f.rfind(':') {
                let path_part = &f[..colon];
                let range_part = &f[colon + 1..];
                if let Some(dash) = range_part.find('-')
                    && let (Ok(s), Ok(e)) = (
                        range_part[..dash].parse::<usize>(),
                        range_part[dash + 1..].parse::<usize>(),
                    )
                {
                    file = normalize_path(path_part);
                    line_range = Some((s, e));
                    continue;
                }
            }
            file = normalize_path(f);
        } else if let Some(s) = line.strip_prefix("Signature: ") {
            signature = Some(s.trim().to_owned());
        } else if let Some(r) = line.strip_prefix("Returns: ") {
            let r = r.trim();
            if !r.is_empty() {
                return_type = Some(r.to_owned());
            }
        } else if let Some(c) = line.strip_prefix("Calls: ") {
            for token in c.split(", ").map(str::trim) {
                let (name, line_num) = token.rfind('@')
                    .and_then(|at| token[at + 1..].parse::<u32>().ok().map(|n| (&token[..at], n)))
                    .unwrap_or((token, 0));
                calls.push(name.to_owned());
                call_lines.push(line_num);
            }
        } else if let Some(t) = line.strip_prefix("Types: ") {
            types = t.split(", ").map(|s| s.trim().to_owned()).collect();
        } else if let Some(f) = line.strip_prefix("Flows: ") {
            for (param, callee) in parse_pairs(f, "\u{2192}") {
                param_flows.push((param, 0, callee, 0, 0));
            }
        } else if let Some(s) = line.strip_prefix("Strings: ") {
            string_args = parse_call_string_args(s);
        } else if let Some(p) = line.strip_prefix("Params: ") {
            param_types = parse_colon_pairs(p);
        } else if let Some(ft) = line.strip_prefix("Fields: ") {
            field_types = parse_colon_pairs(ft);
        } else if let Some(lt) = line.strip_prefix("Locals: ") {
            local_types = parse_colon_pairs(lt);
        } else if let Some(b) = line.strip_prefix("Bindings: ") {
            let_call_bindings = parse_eq_pairs(b);
        } else if let Some(fa) = line.strip_prefix("FieldAccesses: ") {
            field_accesses = parse_dot_pairs(fa);
        } else if let Some(v) = line.strip_prefix("Variants: ") {
            enum_variants = v.split(", ").map(|s| s.trim().to_owned()).collect();
        }
    }

    if file.is_empty() {
        return None;
    }

    Some(ParsedChunk {
        kind, name, file, lines: line_range, signature, calls, call_lines,
        types, string_args, param_flows, param_types, field_types,
        local_types, let_call_bindings, field_accesses, return_type, enum_variants,
        ..Default::default()
    })
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
