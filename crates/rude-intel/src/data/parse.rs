//! Text field parser for code chunks.
//!
//! Parses the structured text field produced by `chunk_code` into a
//! [`ParsedChunk`] struct for structural queries.

/// Structured representation of a code chunk's text field.
///
/// This is the lightweight analysis view parsed from the DB text field,
/// distinct from `rude_chunk::types::CodeChunk` which holds the full
/// tree-sitter parse result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
pub struct ParsedChunk {
    pub kind: String,
    pub name: String,
    pub file: String,
    pub lines: Option<(usize, usize)>,
    pub signature: Option<String>,
    pub calls: Vec<String>,
    /// Source line (1-based) of each call in `calls` (parallel array).
    pub call_lines: Vec<u32>,
    pub types: Vec<String>,
    /// File-level import statements (loaded from payload custom fields).
    #[serde(default)]
    pub imports: Vec<String>,
    /// String literal arguments: (callee, value, line_1based, arg_position).
    #[serde(default)]
    pub string_args: Vec<(String, String, u32, u8)>,
    /// Parameter-to-callee argument flows: (param_name, param_pos, callee, callee_arg, line).
    #[serde(default)]
    pub param_flows: Vec<(String, u8, String, u8, u32)>,
    /// Parameter name → type mappings (e.g., `("dag", "Dag")`).
    #[serde(default)]
    pub param_types: Vec<(String, String)>,
    /// Struct field name → type mappings (e.g., `("name", "String")`).
    #[serde(default)]
    pub field_types: Vec<(String, String)>,
    /// Local variable type annotations (e.g., `("x", "Vec")`).
    #[serde(default)]
    pub local_types: Vec<(String, String)>,
    /// Let-binding-to-call mappings: `(variable_name, callee_name)`.
    /// Used for 1-hop return type propagation.
    #[serde(default)]
    pub let_call_bindings: Vec<(String, String)>,
    /// Return type (e.g., `"Result<Vec<Item>>"`, `"Self"`).
    #[serde(default)]
    pub return_type: Option<String>,
    /// Field accesses (non-call): `(receiver, field_name)`.
    #[serde(default)]
    pub field_accesses: Vec<(String, String)>,
    /// Enum variant names (lowercase, for enum chunks only).
    /// Used to distinguish `Type::Variant(args)` from `Type::method(args)`.
    #[serde(default)]
    pub enum_variants: Vec<String>,
    /// Whether this function has a test attribute (`#[test]`, `@Test`, etc.).
    #[serde(default)]
    pub is_test: bool,
}

impl ParsedChunk {
    /// Convert a `CodeChunk` directly into a `ParsedChunk` (no text re-parsing).
    ///
    /// This is much faster than serializing to text and re-parsing.
    /// Used by `rude add` to pre-build the chunks.bin cache.
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
            lines,
            signature: chunk.signature.clone(),
            calls: chunk.calls.clone(),
            // CodeChunk stores 0-based lines; ParsedChunk uses 1-based (matches text parsing).
            call_lines: chunk.call_lines.iter().map(|l| l + 1).collect(),
            types: chunk.type_refs.clone(),
            imports,
            string_args: chunk.string_args.clone(),
            param_flows: chunk.param_flows.clone(),
            param_types: chunk.param_types.clone(),
            field_types: chunk.field_types.clone(),
            local_types: chunk.local_types.clone(),
            let_call_bindings: chunk.let_call_bindings.clone(),
            return_type: chunk.return_type.clone(),
            field_accesses: chunk.field_accesses.clone(),
            enum_variants: chunk.enum_variants.clone(),
            is_test: chunk.is_test,
        }
    }
}

/// Parse `"name: type, name: type, ..."` into `Vec<(name, type)>`.
fn parse_colon_pairs(s: &str) -> Vec<(String, String)> {
    s.split(", ")
        .filter_map(|tok| {
            let tok = tok.trim();
            let c = tok.find(": ")?;
            Some((tok[..c].to_owned(), tok[c + 2..].to_owned()))
        })
        .collect()
}

/// Parse `"var=callee, ..."` into `Vec<(var, callee)>`.
fn parse_eq_pairs(s: &str) -> Vec<(String, String)> {
    s.split(", ")
        .filter_map(|tok| {
            let tok = tok.trim();
            let eq = tok.find('=')?;
            Some((tok[..eq].to_owned(), tok[eq + 1..].to_owned()))
        })
        .collect()
}

/// Parse `"recv.field, ..."` into `Vec<(recv, field)>`.
fn parse_dot_pairs(s: &str) -> Vec<(String, String)> {
    s.split(", ")
        .filter_map(|tok| {
            let tok = tok.trim();
            let dot = tok.find('.')?;
            Some((tok[..dot].to_owned(), tok[dot + 1..].to_owned()))
        })
        .collect()
}

/// Parse the text field of a code chunk into a [`ParsedChunk`].
///
/// Expected format (first line is `[kind] [vis] name`):
/// ```text
/// [function] pub ParsedChunk::parse
/// File: crates/rude-intel/src/parse.rs:51-120
/// Signature: pub fn parse(text: &str) -> Option<ParsedChunk>
/// Types: ParsedChunk, String
/// Calls: String::new, lines.next
/// ```
pub fn parse_chunk(text: &str) -> Option<ParsedChunk> {
    let mut lines_iter = text.lines();
    let first = lines_iter.next()?;

    // Must start with [kind]
    if !first.starts_with('[') {
        return None;
    }
    let bracket_end = first.find(']')?;
    let kind = first[1..bracket_end].to_owned();
    let rest = first[bracket_end + 1..].trim();

    // Strip optional visibility prefix. impl/trait allow "pub"; others also allow "export".
    // "[impl] VectorIndex for HnswGraph<D>" → "VectorIndex for HnswGraph<D>"
    // "[function] pub StorageEngine::insert" → "StorageEngine::insert"
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
            // Parse "path:start-end" or just "path"
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
            // Parse "name@line" annotations: split off @N suffix
            for token in c.split(", ") {
                let token = token.trim();
                if let Some(at) = token.rfind('@')
                    && let Ok(line_num) = token[at + 1..].parse::<u32>() {
                        calls.push(token[..at].to_owned());
                        call_lines.push(line_num);
                        continue;
                    }
                calls.push(token.to_owned());
                call_lines.push(0);
            }
        } else if let Some(t) = line.strip_prefix("Types: ") {
            types = t.split(", ").map(|s| s.trim().to_owned()).collect();
        } else if let Some(f) = line.strip_prefix("Flows: ") {
            for token in f.split(", ") {
                // Format: "param→callee"
                if let Some(arrow) = token.find('\u{2192}') {
                    let param = token[..arrow].to_owned();
                    let callee = token[arrow + '\u{2192}'.len_utf8()..].to_owned();
                    param_flows.push((param, 0, callee, 0, 0));
                }
            }
        } else if let Some(s) = line.strip_prefix("Strings: ") {
            // Format: Command::new("claude"), env::var("API_KEY")
            for token in s.split(", ") {
                let token = token.trim();
                if let Some(paren) = token.find('(')
                    && let Some(end) = token.rfind(')')
                    && paren < end
                {
                    let callee = token[..paren].to_owned();
                    let inner = &token[paren + 1..end];
                    let value = inner.trim_matches('"').to_owned();
                    string_args.push((callee, value, 0, 0));
                }
            }
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
        kind,
        name,
        file,
        lines: line_range,
        signature,
        calls,
        call_lines,
        types,
        imports: Vec::new(),
        string_args,
        param_flows,
        param_types,
        field_types,
        local_types,
        let_call_bindings,
        field_accesses,
        return_type,
        enum_variants,
        is_test: false, // text parsing doesn't have attribute info; rely on path/name fallback
    })
}

/// Normalize Windows backslashes, strip leading `.\`, and reduce absolute
/// paths to project-relative form (anchored at `crates/` or `src/`).
/// Convert an absolute or mixed-slash path to a project-relative path.
///
/// Uses `PROJECT_ROOT` (set once via [`set_project_root`]) to strip the prefix.
/// Falls back to heuristic anchor detection when no root is set.
pub fn normalize_path(p: &str) -> String {
    let s = p.replace('\\', "/");
    let s = s.strip_prefix("./").unwrap_or(&s);

    // Strip UNC prefix (`//?/` or `\\?\`)
    let s = s.strip_prefix("//?/").unwrap_or(s);

    // Try stripping the project root prefix.
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
                // Use original casing from `s`
                return s[s.len() - rel.len()..].to_owned();
            }
        }
    }

    s.to_owned()
}

use std::sync::OnceLock;

static PROJECT_ROOT: OnceLock<String> = OnceLock::new();

/// Set the project root for path normalization. Should be called once at startup.
/// The root is stored as a forward-slash normalized path without trailing slash.
pub fn set_project_root(root: &std::path::Path) {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut s = canonical.to_string_lossy().replace('\\', "/");
    // Strip UNC prefix
    if let Some(stripped) = s.strip_prefix("//?/") {
        s = stripped.to_owned();
    }
    // Remove trailing slash
    while s.ends_with('/') {
        s.pop();
    }
    let _ = PROJECT_ROOT.set(s);
}
