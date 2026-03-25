use std::collections::HashMap;
use std::ops::ControlFlow;

use rustc_public::CrateDef;
use rustc_public::mir::{MirVisitor, Terminator, TerminatorKind, LocalDecl};
use rustc_public::mir::visit::Location;
use rustc_public::ty::{RigidTy, TyKind, Span, AdtKind};

use crate::output;
use crate::types::{CallEdge, MirChunk};

// ── Span helpers ────────────────────────────────────────────────────

pub fn span_file(span: &Span) -> String {
    span.get_filename().to_string().replace('\\', "/")
}

pub fn span_lines(span: &Span) -> (usize, usize) {
    let info = span.get_lines();
    (info.start_line, info.end_line)
}

pub fn is_local_file(file: &str) -> bool {
    !file.starts_with('/') && !file.contains(":/")
}

// ── Name helpers ────────────────────────────────────────────────────

/// Strip crate prefix, preserving `<` for impl names.
/// "rude_intel::context_cmd::f" → "context_cmd::f"
/// "<rude_db::Foo as Bar>" → "<Foo as Bar>"
pub fn strip_crate_prefix(name: &str) -> String {
    if let Some(inner) = name.strip_prefix('<') {
        if let Some(pos) = inner.find("::") {
            return format!("<{}", &inner[pos + 2..]);
        }
        return name.to_string();
    }
    name.find("::").map_or_else(|| name.to_string(), |pos| name[pos + 2..].to_string())
}

fn adt_kind_str(kind: AdtKind) -> &'static str {
    match kind {
        AdtKind::Enum => "enum",
        AdtKind::Struct | AdtKind::Union => "struct",
    }
}

fn impl_self_ty(imp: &rustc_public::ty::ImplDef) -> Option<rustc_public::ty::Ty> {
    imp.trait_impl().value.args().0.first().and_then(|arg| match arg {
        rustc_public::ty::GenericArgKind::Type(ty) => Some(*ty),
        _ => None,
    })
}

fn make_chunk(name: String, file: String, kind: &str, span: &Span) -> MirChunk {
    let (start, end) = span_lines(span);
    MirChunk {
        name, file, kind: kind.to_string(),
        start_line: start, end_line: end,
        signature: None, visibility: String::new(),
        is_test: false, body: String::new(),
        calls: String::new(), type_refs: String::new(),
    }
}

/// Clean MIR signature for display:
/// - Strip trailing `{`
/// - Remove `_N: ` parameter names → keep type only
/// - Simplify module paths: `graph::CallGraph` → `CallGraph`
fn clean_mir_signature(raw: &str) -> String {
    let s = raw.trim_end_matches(|c: char| c == '{' || c.is_whitespace());

    // Replace `_N: Type` with just `Type` in parameter list
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut in_params = false;

    while let Some(c) = chars.next() {
        if c == '(' { in_params = true; result.push(c); continue; }
        if c == ')' { in_params = false; result.push(c); continue; }

        if in_params && c == '_' {
            // Check if this is `_N: ` pattern
            let mut digits = String::new();
            while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                digits.push(chars.next().unwrap());
            }
            if !digits.is_empty() && chars.peek() == Some(&':') {
                chars.next(); // skip ':'
                if chars.peek() == Some(&' ') { chars.next(); } // skip space
                continue; // skip the `_N: ` prefix
            }
            result.push('_');
            result.push_str(&digits);
        } else {
            result.push(c);
        }
    }

    // Simplify paths: `module::Type` → `Type` (keep last segment)
    let simplified = result
        .split(|c: char| c == '(' || c == ')' || c == ',' || c == '>' || c == '<')
        .fold(result.clone(), |acc, _| acc);

    // Simpler approach: regex-like replacement of `word::word::Type` → `Type`
    let mut out = String::new();
    let mut i = 0;
    let bytes = result.as_bytes();
    while i < bytes.len() {
        // Find sequences of `ident::ident::...::LastIdent` and keep only LastIdent
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            let mut last_segment_start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            // Check for ::
            while i + 1 < bytes.len() && bytes[i] == b':' && bytes[i + 1] == b':' {
                i += 2;
                last_segment_start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
            }
            out.push_str(&result[last_segment_start..i]);
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }

    out
}

/// Build signature for struct/enum from AdtDef API
fn build_adt_signature(name: &str, adt: &rustc_public::ty::AdtDef) -> String {
    use rustc_public::CrateDef;
    match adt.kind() {
        AdtKind::Struct => {
            let variants = adt.variants();
            if let Some(v) = variants.first() {
                let fields: Vec<String> = v.fields().iter()
                    .map(|f| format!("{}: {}", f.name, clean_ty_name(&f.ty())))
                    .collect();
                if fields.is_empty() {
                    format!("struct {name}")
                } else {
                    format!("struct {name} {{ {} }}", fields.join(", "))
                }
            } else {
                format!("struct {name}")
            }
        }
        AdtKind::Enum => {
            let variants: Vec<String> = adt.variants_iter()
                .map(|v| v.name())
                .collect();
            format!("enum {name} {{ {} }}", variants.join(", "))
        }
        AdtKind::Union => format!("union {name}"),
    }
}

/// Simplify Ty debug output to readable type name
fn clean_ty_name(ty: &rustc_public::ty::Ty) -> String {
    let raw = format!("{ty:?}");
    // Extract kind from `Ty { id: N, kind: ... }`
    if let Some(start) = raw.find("kind: ") {
        let rest = &raw[start + 6..];
        // Quick heuristic: use the type's debug but strip Ty{} wrapper
        if rest.contains("Adt(AdtDef(DefId") {
            // Extract name from AdtDef
            if let Some(name_start) = rest.find("name: \"") {
                let after = &rest[name_start + 7..];
                if let Some(name_end) = after.find('"') {
                    let full_name = &after[..name_end];
                    return full_name.rsplit("::").next().unwrap_or(full_name).to_string();
                }
            }
        }
        if rest.starts_with("RigidTy(Str)") { return "String".into(); }
        if rest.starts_with("RigidTy(Bool)") { return "bool".into(); }
        if rest.contains("Uint(Usize)") { return "usize".into(); }
        if rest.contains("Uint(U64)") { return "u64".into(); }
        if rest.contains("Uint(U32)") { return "u32".into(); }
        if rest.contains("Uint(U8)") { return "u8".into(); }
        if rest.contains("Int(Isize)") { return "isize".into(); }
        if rest.contains("Int(I64)") { return "i64".into(); }
        if rest.contains("Int(I32)") { return "i32".into(); }
        if rest.contains("Ref(") { return "&...".into(); }
    }
    "...".into()
}

fn is_test_fn(filename: &str, name: &str, is_test_target: bool) -> bool {
    is_test_target
        || filename.contains("/tests/") || filename.contains("\\tests\\")
        || name.starts_with("test_") || name.contains("::test_")
}

// ── Call edge visitor ───────────────────────────────────────────────

struct CallExtractor<'a> {
    caller_name: String,
    caller_file: String,
    locals: &'a [LocalDecl],
    edges: &'a mut Vec<CallEdge>,
    name_cache: &'a mut HashMap<String, String>,
}

impl MirVisitor for CallExtractor<'_> {
    fn visit_terminator(&mut self, term: &Terminator, loc: Location) {
        if let TerminatorKind::Call { ref func, ref args, .. } = term.kind {
            // Direct call
            if let Ok(op_ty) = func.ty(self.locals) {
                if let TyKind::RigidTy(RigidTy::FnDef(def, _)) = op_ty.kind() {
                    self.push_edge(&def, &term.span);
                }
            }
            // Function references passed as arguments (e.g. spawn(fn))
            for arg in args {
                if let Ok(arg_ty) = arg.ty(self.locals) {
                    if let TyKind::RigidTy(RigidTy::FnDef(def, _)) = arg_ty.kind() {
                        self.push_edge(&def, &term.span);
                    }
                }
            }
        }
        self.super_terminator(term, loc);
    }
}

impl CallExtractor<'_> {
    fn push_edge(&mut self, def: &rustc_public::ty::FnDef, call_span: &Span) {
        let raw = def.name().to_string();
        let callee_name = self.name_cache
            .entry(raw.clone())
            .or_insert_with(|| strip_crate_prefix(&raw))
            .clone();

        let callee_span = def.span();
        let callee_file = span_file(&callee_span);
        let (call_line, _) = span_lines(call_span);

        let is_external = !is_local_file(&callee_file);
        let (cf, cs) = if is_external {
            (String::new(), 0)
        } else {
            let (start, _) = span_lines(&callee_span);
            (callee_file, start)
        };

        self.edges.push(CallEdge {
            caller: self.caller_name.clone(),
            caller_file: self.caller_file.clone(),
            caller_kind: "fn".to_string(),
            callee: callee_name,
            callee_file: cf,
            callee_start_line: cs,
            line: call_line,
            is_local: !is_external,
        });
    }
}

// ── Main extraction ─────────────────────────────────────────────────

pub fn extract_all(is_test_target: bool, json: bool, db_path: &Option<String>) -> ControlFlow<()> {
    let _span = tracing::info_span!("extract_all").entered();
    let krate = rustc_public::local_crate();
    let crate_name = krate.name.to_string();

    let mut edges: Vec<CallEdge> = Vec::new();
    let mut chunks: Vec<MirChunk> = Vec::new();
    let mut fn_count: usize = 0;
    let mut name_cache: HashMap<String, String> = HashMap::new();

    // Phase 1: type chunks (trait/impl/struct/enum)
    let mut seen_types = collect_type_chunks(&krate, &mut chunks);

    // Phase 2: function chunks + call edges
    for fn_def in krate.fn_defs() {
        let name_str = fn_def.name().to_string();
        if name_str.contains("{closure") { continue; }
        let item = rustc_public::CrateItem(fn_def.def_id());
        if !item.has_body() { continue; }
        fn_count += 1;
        let body = item.body().unwrap();
        let name = strip_crate_prefix(&item.name());
        let filename = span_file(&body.span);
        let (start_line, end_line) = span_lines(&body.span);

        // Extract signature from fn_sig (fallback; source signature is primary)
        let signature = {
            use rustc_public::ty::{RigidTy, TyKind};
            if let TyKind::RigidTy(RigidTy::FnDef(def, _)) = item.ty().kind() {
                let sig = def.fn_sig().value;
                let params: Vec<String> = sig.inputs().iter()
                    .map(|t| clean_mir_signature(&format!("{t:?}")))
                    .collect();
                let ret = clean_mir_signature(&format!("{:?}", sig.output()));
                let short_name = name.rsplit("::").next().unwrap_or(&name);
                if ret == "()" {
                    Some(format!("fn {short_name}({})", params.join(", ")))
                } else {
                    Some(format!("fn {short_name}({}) -> {ret}", params.join(", ")))
                }
            } else { None }
        };

        // Extract call edges
        let mut extractor = CallExtractor {
            caller_name: name.clone(),
            caller_file: filename.clone(),
            locals: body.locals(),
            edges: &mut edges,
            name_cache: &mut name_cache,
        };
        extractor.visit_body(&body);

        chunks.push(MirChunk {
            name, file: filename.clone(), kind: "fn".to_string(),
            start_line, end_line, signature, visibility: String::new(),
            is_test: is_test_fn(&filename, &chunks.last().map(|c| c.name.as_str()).unwrap_or(""), is_test_target),
            body: String::new(), calls: String::new(), type_refs: String::new(),
        });

        // Scan local variable types for additional struct/enum
        for local in body.locals() {
            try_add_adt(&local.ty, &mut seen_types, &mut chunks);
        }
    }

    // Fill calls per chunk
    fill_chunk_calls(&mut chunks, &edges);

    // Output
    output::write_results(&crate_name, &edges, &chunks, fn_count, json, db_path);

    ControlFlow::Continue(())
}

fn collect_type_chunks(krate: &rustc_public::Crate, chunks: &mut Vec<MirChunk>) -> std::collections::HashSet<String> {
    let _phase = tracing::info_span!("type_chunks").entered();
    let mut seen_types: std::collections::HashSet<String> = std::collections::HashSet::new();

    for t in krate.trait_decls() {
        let file = span_file(&t.span());
        if !is_local_file(&file) { continue; }
        chunks.push(make_chunk(strip_crate_prefix(&t.name()), file, "trait", &t.span()));
    }

    for imp in krate.trait_impls() {
        let file = span_file(&imp.span());
        if !is_local_file(&file) { continue; }
        chunks.push(make_chunk(strip_crate_prefix(&imp.name()), file, "impl", &imp.span()));

        if let Some(ty) = impl_self_ty(&imp) {
            try_add_adt(&ty, &mut seen_types, chunks);
        }
    }

    // Scan all local items' types for additional ADTs (structs without trait impls)
    for fn_def in krate.fn_defs() {
        let item = rustc_public::CrateItem(fn_def.def_id());
        let ty = item.ty();
        // Direct ADT (e.g. Ctor returns the struct type)
        try_add_adt(&ty, &mut seen_types, chunks);

        // Scan function signature params + return type
        if let TyKind::RigidTy(RigidTy::FnDef(def, _)) = ty.kind() {
            let sig = def.fn_sig().value;
            for param in sig.inputs() {
                try_add_adt(param, &mut seen_types, chunks);
                // Also check inside &T, &mut T
                if let TyKind::RigidTy(RigidTy::Ref(_, inner, _)) = param.kind() {
                    try_add_adt(&inner, &mut seen_types, chunks);
                }
            }
            try_add_adt(&sig.output(), &mut seen_types, chunks);
            if let TyKind::RigidTy(RigidTy::Ref(_, inner, _)) = sig.output().kind() {
                try_add_adt(&inner, &mut seen_types, chunks);
            }
        }
    }

    seen_types
}

/// If ty is an ADT with a local source file, add a struct/enum chunk.
/// Also recursively scans field types to find nested ADTs.
fn try_add_adt(
    ty: &rustc_public::ty::Ty,
    seen: &mut std::collections::HashSet<String>,
    chunks: &mut Vec<MirChunk>,
) {
    // Unwrap references/slices to get inner type
    let inner = match ty.kind() {
        TyKind::RigidTy(RigidTy::Ref(_, inner, _)) => inner,
        TyKind::RigidTy(RigidTy::Slice(inner)) => inner,
        _ => *ty,
    };

    if let TyKind::RigidTy(RigidTy::Adt(adt_def, _)) = inner.kind() {
        let name = strip_crate_prefix(&adt_def.name());
        let file = span_file(&adt_def.span());
        if is_local_file(&file) && seen.insert(name.clone()) {
            let sig = build_adt_signature(&name, &adt_def);
            let kind_str = adt_kind_str(adt_def.kind());
            let (s, e) = span_lines(&adt_def.span());
            chunks.push(MirChunk {
                name, file, kind: kind_str.to_string(),
                start_line: s, end_line: e,
                signature: Some(sig), visibility: String::new(),
                is_test: false, body: String::new(),
                calls: String::new(), type_refs: String::new(),
            });
            // Recurse into field types
            for variant in adt_def.variants() {
                for field in variant.fields() {
                    try_add_adt(&field.ty(), seen, chunks);
                }
            }
        }
    }
}

fn fill_chunk_calls(chunks: &mut [MirChunk], edges: &[CallEdge]) {
    let mut by_caller: HashMap<&str, Vec<String>> = HashMap::new();
    for e in edges {
        by_caller.entry(&e.caller).or_default().push(format!("{}@{}", e.callee, e.line));
    }
    for c in chunks.iter_mut() {
        if let Some(calls) = by_caller.remove(c.name.as_str()) {
            c.calls = calls.join(", ");
        }
    }
}
