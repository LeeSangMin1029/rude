use std::collections::HashMap;
use std::fmt;

use rude_db::PayloadValue;

use crate::minhash;

/// A sub-block within a code chunk, split at control structure boundaries.
///
/// Used for fine-grained (intra-function) clone detection: two functions may
/// not be duplicates overall, but share identical internal blocks.
#[derive(Debug, Clone)]
pub struct SubBlock {
    /// Byte offset in source file.
    pub start_byte: usize,
    pub end_byte: usize,
    /// Line numbers (0-based).
    pub start_line: usize,
    pub end_line: usize,
    /// Structural AST hash (identifiers normalized).
    pub ast_hash: u64,
    /// Normalized body text hash.
    pub body_hash: u64,
}

impl fmt::Display for SubBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "L{}-{}", self.start_line + 1, self.end_line + 1)
    }
}

/// Configuration for code chunking.
#[derive(Debug, Clone)]
pub struct CodeChunkConfig {
    /// Minimum lines for a chunk to be included.
    pub min_lines: usize,
    /// Extract file-level `use` statements.
    pub extract_imports: bool,
    /// Extract function calls from bodies.
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

/// Kind of code node extracted by tree-sitter.
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
    /// String label for payload storage.
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

/// A semantic code chunk extracted via tree-sitter.
#[derive(Debug, Clone)]
pub struct CodeChunk {
    /// The raw source code text.
    pub text: String,
    /// Kind of code node.
    pub kind: CodeNodeKind,
    /// Symbol name (e.g., `process_payment`, `PaymentIntent`).
    pub name: String,
    /// Function signature (params + return), if applicable.
    pub signature: Option<String>,
    /// Doc comment text, if any.
    pub doc_comment: Option<String>,
    /// Visibility: `"pub"`, `"pub(crate)"`, `""`.
    pub visibility: String,
    /// Start line (0-based).
    pub start_line: usize,
    /// End line (0-based).
    pub end_line: usize,
    /// Byte offsets in source.
    pub start_byte: usize,
    pub end_byte: usize,
    /// Sequential chunk index within the file.
    pub chunk_index: usize,
    /// File-level import statements.
    pub imports: Vec<String>,
    /// Function calls within the body.
    pub calls: Vec<String>,
    /// Source line (0-based) of each call in `calls` (parallel array).
    pub call_lines: Vec<u32>,
    /// Type names referenced in signature and body.
    pub type_refs: Vec<String>,
    /// Parameter name-type pairs (e.g., `[("amount", "f64")]`).
    pub param_types: Vec<(String, String)>,
    /// Struct field name-type pairs (e.g., `[("name", "String")]`).
    pub field_types: Vec<(String, String)>,
    /// Return type string (e.g., `"Result<Vec<Item>>"`).
    pub return_type: Option<String>,
    /// Structural AST hash for clone detection (0 = not computed).
    pub ast_hash: u64,
    /// Normalized body text hash for exact-logic clone detection (0 = not computed).
    pub body_hash: u64,
    /// Sub-blocks split at control structure boundaries for fine-grained clone detection.
    pub sub_blocks: Vec<SubBlock>,
    /// String literal arguments found in function calls: `(callee, value, line, arg_pos)`.
    pub string_args: Vec<(String, String, u32, u8)>,
    /// Parameter-to-callee argument flows: `(param_name, param_pos, callee, callee_arg, line)`.
    pub param_flows: Vec<(String, u8, String, u8, u32)>,
    /// Local variable type annotations: `(variable_name, type_name)`.
    /// Collected from `let x: Type = ...` patterns.
    pub local_types: Vec<(String, String)>,
    /// Let-binding-to-call mappings: `(variable_name, callee_name)`.
    /// Collected from `let x = some_fn()` / `let x = some_fn()?` patterns.
    /// Used for 1-hop return type propagation in the call graph.
    pub let_call_bindings: Vec<(String, String)>,
    /// Field accesses (non-call): `(receiver, field_name)`.
    /// Collected from `payload.source`, `self.engine`, `node.incoming` etc.
    /// Used for field-level blast radius analysis.
    pub field_accesses: Vec<(String, String)>,
    /// Enum variant names (for enum chunks only).
    /// Used to distinguish `Type::Variant(args)` from `Type::method(args)`.
    pub enum_variants: Vec<String>,
    /// Whether this function has a test attribute (`#[test]`, `@Test`, etc.).
    /// Detected from tree-sitter AST — language-specific.
    pub is_test: bool,
}

impl CodeChunk {
    /// Build text optimized for embedding (semantic search).
    ///
    /// Includes doc comment, signature, calls, called_by — not the full body.
    /// Use `called_by` to inject reverse-reference data from cross-file analysis.
    pub fn to_embed_text(&self, file_path: &str, called_by: &[String]) -> String {
        // Helper: push "Label: items" if non-empty
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

    /// Convert code metadata to `payload.custom` fields.
    ///
    /// Pass `called_by` from cross-file reverse-reference analysis.
    pub fn to_custom_fields(&self, called_by: &[String]) -> HashMap<String, PayloadValue> {
        // Helper: insert StringList only when non-empty
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

        // MinHash token fingerprint for near-duplicate detection
        let tokens = minhash::code_tokens(&self.text);
        if tokens.len() >= 10 {
            ins_str(&mut c, "minhash", minhash::minhash_to_hex(&minhash::minhash_signature(&tokens, minhash::MINHASH_K)));
        }

        c
    }
}
