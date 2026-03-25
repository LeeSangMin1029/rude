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
}

impl ParsedChunk {
    /// Build embed text for DB storage (replaces CodeChunk::to_embed_text).
    pub fn to_embed_text(&self, called_by: &[String]) -> String {
        fn push_list(parts: &mut Vec<String>, label: &str, items: &[String]) {
            if !items.is_empty() {
                parts.push(format!("{label}: {}", items.join(", ")));
            }
        }
        fn pairs_to_strings(pairs: &[(String, String)], sep: &str) -> Vec<String> {
            pairs.iter().map(|(a, b)| format!("{a}{sep}{b}")).collect()
        }

        let mut parts = Vec::new();

        let vis = if self.visibility.is_empty() { String::new() } else { format!("{} ", self.visibility) };
        parts.push(format!("[{}] {vis}{}", self.kind, self.name));

        if let Some((s, e)) = self.lines {
            parts.push(format!("File: {}:{s}-{e}", self.file));
        } else {
            parts.push(format!("File: {}", self.file));
        }

        if let Some(ref sig) = self.signature { parts.push(format!("Signature: {sig}")); }

        push_list(&mut parts, "Params",  &pairs_to_strings(&self.param_types, ": "));
        push_list(&mut parts, "Fields",  &pairs_to_strings(&self.field_types, ": "));

        if let Some(ref ret) = self.return_type { parts.push(format!("Returns: {ret}")); }

        push_list(&mut parts, "Types", &self.types);

        if !self.calls.is_empty() {
            let annotated: Vec<String> = self.calls.iter().enumerate()
                .map(|(i, c)| match self.call_lines.get(i) {
                    Some(&line) if line > 0 => format!("{c}@{line}"),
                    _ => c.clone(),
                })
                .collect();
            push_list(&mut parts, "Calls", &annotated);
        }

        push_list(&mut parts, "Strings",
            &self.string_args.iter().map(|(c, v, _, _)| format!("{c}(\"{v}\")")).collect::<Vec<_>>());
        push_list(&mut parts, "Flows",
            &self.param_flows.iter().map(|(p, _, c, _, _)| format!("{p}\u{2192}{c}")).collect::<Vec<_>>());
        push_list(&mut parts, "Locals",  &pairs_to_strings(&self.local_types, ": "));
        push_list(&mut parts, "Bindings",
            &self.let_call_bindings.iter().map(|(v, c)| format!("{v}={c}")).collect::<Vec<_>>());
        push_list(&mut parts, "FieldAccesses",
            &self.field_accesses.iter().map(|(r, f)| format!("{r}.{f}")).collect::<Vec<_>>());
        push_list(&mut parts, "Variants", &self.enum_variants);
        push_list(&mut parts, "Called by", called_by);

        parts.join("\n")
    }

    /// Build custom payload fields for DB storage (replaces CodeChunk::to_custom_fields).
    pub fn to_custom_fields(&self, called_by: &[String]) -> std::collections::HashMap<String, rude_db::PayloadValue> {
        use rude_db::PayloadValue;
        use std::collections::HashMap;

        fn ins_list(m: &mut HashMap<String, PayloadValue>, key: &str, v: Vec<String>) {
            if !v.is_empty() { m.insert(key.to_owned(), PayloadValue::StringList(v)); }
        }
        fn ins_str(m: &mut HashMap<String, PayloadValue>, key: &str, v: String) {
            m.insert(key.to_owned(), PayloadValue::String(v));
        }

        let mut c = HashMap::new();

        ins_str(&mut c, "kind",       self.kind.clone());
        ins_str(&mut c, "name",       self.name.clone());
        ins_str(&mut c, "visibility", self.visibility.clone());

        if let Some((s, _e)) = self.lines {
            c.insert("start_line".to_owned(), PayloadValue::Integer(s as i64));
        }
        if let Some((_s, e)) = self.lines {
            c.insert("end_line".to_owned(), PayloadValue::Integer(e as i64));
        }

        if let Some(ref s) = self.signature   { ins_str(&mut c, "signature",    s.clone()); }
        if let Some(ref r) = self.return_type  { ins_str(&mut c, "return_type",  r.clone()); }

        ins_list(&mut c, "calls",     self.calls.clone());
        ins_list(&mut c, "imports",   self.imports.clone());
        ins_list(&mut c, "called_by", called_by.to_vec());
        ins_list(&mut c, "type_refs", self.types.clone());
        ins_list(&mut c, "string_args",
            self.string_args.iter().map(|(cl, v, l, p)| format!("{cl}\t{v}\t{l}\t{p}")).collect());
        ins_list(&mut c, "param_flows",
            self.param_flows.iter().map(|(pn, pp, cl, ca, l)| format!("{pn}\t{pp}\t{cl}\t{ca}\t{l}")).collect());
        ins_list(&mut c, "local_types",
            self.local_types.iter().map(|(n, t)| format!("{n}\t{t}")).collect());

        let tokens = crate::minhash::code_tokens(&self.text);
        if tokens.len() >= 10 {
            ins_str(&mut c, "minhash", crate::minhash::minhash_to_hex(
                &crate::minhash::minhash_signature(&tokens, crate::minhash::MINHASH_K),
            ));
        }

        c
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
