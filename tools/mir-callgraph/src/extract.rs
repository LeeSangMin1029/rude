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
    collect_type_chunks(&krate, &mut chunks);

    // Phase 2: function chunks + call edges
    for item in rustc_public::all_local_items() {
        let kind_debug = format!("{:?}", item.kind());
        let name_str = item.name().to_string();
        // Skip: non-fn, closures, no body
        if kind_debug != "Fn" || name_str.contains("{closure") || !item.has_body() {
            continue;
        }
        fn_count += 1;
        let body = item.body().unwrap();
        let name = strip_crate_prefix(&item.name());
        let filename = span_file(&body.span);
        let (start_line, end_line) = span_lines(&body.span);

        // Extract call edges
        let mut extractor = CallExtractor {
            caller_name: name.clone(),
            caller_file: filename.clone(),
            locals: body.locals(),
            edges: &mut edges,
            name_cache: &mut name_cache,
        };
        extractor.visit_body(&body);

        let kind = "fn";
        chunks.push(MirChunk {
            name, file: filename.clone(), kind: kind.to_string(),
            start_line, end_line, signature: None, visibility: String::new(),
            is_test: is_test_fn(&filename, &chunks.last().map(|c| c.name.as_str()).unwrap_or(""), is_test_target),
            body: String::new(), calls: String::new(), type_refs: String::new(),
        });
    }

    // Fill calls per chunk
    fill_chunk_calls(&mut chunks, &edges);

    // Output
    output::write_results(&crate_name, &edges, &chunks, fn_count, json, db_path);

    ControlFlow::Continue(())
}

fn collect_type_chunks(krate: &rustc_public::Crate, chunks: &mut Vec<MirChunk>) {
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
            if let TyKind::RigidTy(RigidTy::Adt(adt_def, _)) = ty.kind() {
                let type_name = strip_crate_prefix(&adt_def.name());
                let adt_file = span_file(&adt_def.span());
                if is_local_file(&adt_file) && seen_types.insert(type_name.clone()) {
                    chunks.push(make_chunk(type_name, adt_file, adt_kind_str(adt_def.kind()), &adt_def.span()));
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
