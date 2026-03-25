use std::collections::HashMap;
use std::fmt;

use rude_db::PayloadValue;

use crate::data::minhash;

#[derive(Debug, Clone)]
pub struct SubBlock {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub ast_hash: u64,
    pub body_hash: u64,
}

impl fmt::Display for SubBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "L{}-{}", self.start_line + 1, self.end_line + 1)
    }
}

#[derive(Debug, Clone)]
pub struct CodeChunkConfig {
    pub min_lines: usize,
    pub extract_imports: bool,
    pub extract_calls: bool,
}

impl Default for CodeChunkConfig {
    fn default() -> Self {
        Self {
            min_lines: 2,
            extract_imports: true,
            extract_calls: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeNodeKind {
    Function,
    Struct,
    Enum,
    Impl,
    Trait,
    TypeAlias,
    Const,
    Static,
    Module,
    MacroDefinition,
    Class,
    Interface,
}

impl CodeNodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Impl => "impl",
            Self::Trait => "trait",
            Self::TypeAlias => "type_alias",
            Self::Const => "const",
            Self::Static => "static",
            Self::Module => "module",
            Self::MacroDefinition => "macro",
            Self::Class => "class",
            Self::Interface => "interface",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodeChunk {
    pub text: String,
    pub kind: CodeNodeKind,
    pub name: String,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub visibility: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub chunk_index: usize,
    pub imports: Vec<String>,
    pub calls: Vec<String>,
    pub call_lines: Vec<u32>,
    pub type_refs: Vec<String>,
    pub param_types: Vec<(String, String)>,
    pub field_types: Vec<(String, String)>,
    pub return_type: Option<String>,
    pub ast_hash: u64,
    pub body_hash: u64,
    pub sub_blocks: Vec<SubBlock>,
    pub string_args: Vec<(String, String, u32, u8)>,
    pub param_flows: Vec<(String, u8, String, u8, u32)>,
    pub local_types: Vec<(String, String)>,
    pub let_call_bindings: Vec<(String, String)>,
    pub field_accesses: Vec<(String, String)>,
    pub enum_variants: Vec<String>,
    pub is_test: bool,
}

impl CodeChunk {
    pub fn to_embed_text(&self, file_path: &str, called_by: &[String]) -> String {
        fn push_list(parts: &mut Vec<String>, label: &str, items: &[String]) {
            if !items.is_empty() {
                parts.push(format!("{label}: {}", items.join(", ")));
            }
        }
        fn pairs_to_strings<A: AsRef<str>, B: AsRef<str>>(
            pairs: &[(A, B)],
            sep: &str,
        ) -> Vec<String> {
            pairs.iter().map(|(a, b)| format!("{}{sep}{}", a.as_ref(), b.as_ref())).collect()
        }

        let mut parts = Vec::new();

        let vis = if self.visibility.is_empty() { String::new() } else { format!("{} ", self.visibility) };
        parts.push(format!("[{}] {vis}{}", self.kind.as_str(), self.name));
        parts.push(format!("File: {file_path}:{}-{}", self.start_line + 1, self.end_line + 1));

        if let Some(ref doc) = self.doc_comment { parts.push(doc.clone()); }
        if let Some(ref sig) = self.signature    { parts.push(format!("Signature: {sig}")); }

        push_list(&mut parts, "Params",       &pairs_to_strings(&self.param_types, ": "));
        push_list(&mut parts, "Fields",       &pairs_to_strings(&self.field_types, ": "));

        if let Some(ref ret) = self.return_type { parts.push(format!("Returns: {ret}")); }

        push_list(&mut parts, "Types", &self.type_refs);

        if !self.calls.is_empty() {
            let annotated: Vec<String> = self.calls.iter().enumerate()
                .map(|(i, c)| match self.call_lines.get(i) {
                    Some(&line) => format!("{c}@{}", line + 1),
                    None        => c.clone(),
                })
                .collect();
            push_list(&mut parts, "Calls", &annotated);
        }

        push_list(&mut parts, "Strings",
            &self.string_args.iter().map(|(c, v, _, _)| format!("{c}(\"{v}\")")).collect::<Vec<_>>());
        push_list(&mut parts, "Flows",
            &self.param_flows.iter().map(|(p, _, c, _, _)| format!("{p}\u{2192}{c}")).collect::<Vec<_>>());
        push_list(&mut parts, "Locals",       &pairs_to_strings(&self.local_types, ": "));
        push_list(&mut parts, "Bindings",
            &self.let_call_bindings.iter().map(|(v, c)| format!("{v}={c}")).collect::<Vec<_>>());
        push_list(&mut parts, "FieldAccesses",
            &self.field_accesses.iter().map(|(r, f)| format!("{r}.{f}")).collect::<Vec<_>>());
        push_list(&mut parts, "Variants",     &self.enum_variants);
        push_list(&mut parts, "Called by",    called_by);

        parts.join("\n")
    }

    pub fn to_custom_fields(&self, called_by: &[String]) -> HashMap<String, PayloadValue> {
        fn ins_list(m: &mut HashMap<String, PayloadValue>, key: &str, v: Vec<String>) {
            if !v.is_empty() { m.insert(key.to_owned(), PayloadValue::StringList(v)); }
        }
        fn ins_str(m: &mut HashMap<String, PayloadValue>, key: &str, v: String) {
            m.insert(key.to_owned(), PayloadValue::String(v));
        }

        let mut c = HashMap::new();

        ins_str(&mut c, "kind",       self.kind.as_str().to_owned());
        ins_str(&mut c, "name",       self.name.clone());
        ins_str(&mut c, "visibility", self.visibility.clone());
        c.insert("start_line".to_owned(), PayloadValue::Integer(i64::try_from(self.start_line).unwrap_or(0)));
        c.insert("end_line".to_owned(),   PayloadValue::Integer(i64::try_from(self.end_line).unwrap_or(0)));

        if let Some(ref s) = self.signature  { ins_str(&mut c, "signature",   s.clone()); }
        if let Some(ref d) = self.doc_comment { ins_str(&mut c, "doc",        d.clone()); }
        if let Some(ref r) = self.return_type { ins_str(&mut c, "return_type", r.clone()); }

        ins_list(&mut c, "calls",      self.calls.clone());
        ins_list(&mut c, "imports",    self.imports.clone());
        ins_list(&mut c, "called_by",  called_by.to_vec());
        ins_list(&mut c, "type_refs",  self.type_refs.clone());
        ins_list(&mut c, "string_args",
            self.string_args.iter().map(|(cl, v, l, p)| format!("{cl}\t{v}\t{l}\t{p}")).collect());
        ins_list(&mut c, "param_flows",
            self.param_flows.iter().map(|(pn, pp, cl, ca, l)| format!("{pn}\t{pp}\t{cl}\t{ca}\t{l}")).collect());
        ins_list(&mut c, "local_types",
            self.local_types.iter().map(|(n, t)| format!("{n}\t{t}")).collect());
        ins_list(&mut c, "sub_block_hashes",
            self.sub_blocks.iter().map(|sb| format!("{:x}:{}-{}", sb.ast_hash, sb.start_line, sb.end_line)).collect());

        #[expect(clippy::cast_possible_wrap, reason = "hash bits reinterpreted")]
        if self.ast_hash  != 0 { c.insert("ast_hash".to_owned(),  PayloadValue::Integer(self.ast_hash  as i64)); }
        #[expect(clippy::cast_possible_wrap, reason = "hash bits reinterpreted")]
        if self.body_hash != 0 { c.insert("body_hash".to_owned(), PayloadValue::Integer(self.body_hash as i64)); }

        let tokens = minhash::code_tokens(&self.text);
        if tokens.len() >= 10 {
            ins_str(&mut c, "minhash", minhash::minhash_to_hex(&minhash::minhash_signature(&tokens, minhash::MINHASH_K)));
        }

        c
    }
}
