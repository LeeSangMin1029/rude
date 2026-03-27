use std::collections::HashMap;
use std::ops::ControlFlow;

use rustc_public::CrateDef;
use rustc_public::mir::{MirVisitor, Terminator, TerminatorKind, LocalDecl};
use rustc_public::mir::visit::Location;
use rustc_public::ty::{RigidTy, TyKind, Span, AdtKind};

use crate::output;
use crate::types::{CallEdge, MirChunk, UseItem, UseDep};

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


/// Build signature for struct/enum from AdtDef API
fn build_adt_signature(name: &str, adt: &rustc_public::ty::AdtDef) -> String {
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
    match ty.kind() {
        TyKind::RigidTy(RigidTy::Adt(def, _)) => {
            def.name().rsplit("::").next().unwrap_or(&def.name()).to_string()
        }
        TyKind::RigidTy(RigidTy::Str) => "str".into(),
        TyKind::RigidTy(RigidTy::Bool) => "bool".into(),
        TyKind::RigidTy(RigidTy::Char) => "char".into(),
        TyKind::RigidTy(RigidTy::Int(i)) => format!("{i:?}").to_lowercase(),
        TyKind::RigidTy(RigidTy::Uint(u)) => format!("{u:?}").to_lowercase(),
        TyKind::RigidTy(RigidTy::Float(f)) => format!("{f:?}").to_lowercase(),
        TyKind::RigidTy(RigidTy::Ref(_, inner, _)) => format!("&{}", clean_ty_name(&inner)),
        TyKind::RigidTy(RigidTy::Slice(inner)) => format!("[{}]", clean_ty_name(&inner)),
        TyKind::RigidTy(RigidTy::Tuple(ts)) if ts.is_empty() => "()".into(),
        TyKind::RigidTy(RigidTy::Tuple(ts)) => {
            format!("({})", ts.iter().map(|t| clean_ty_name(t)).collect::<Vec<_>>().join(", "))
        }
        _ => "_".into(),
    }
}

fn is_test_fn(filename: &str, name: &str, _is_test_target: bool) -> bool {
    filename.contains("/tests/") || filename.contains("\\tests\\")
        || name.starts_with("test_") || name.contains("::test_")
        || name.contains("::tests::")
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
    let t_all = std::time::Instant::now();
    let krate = rustc_public::local_crate();
    let crate_name = krate.name.to_string();

    let mut edges: Vec<CallEdge> = Vec::new();
    let mut chunks: Vec<MirChunk> = Vec::new();
    let mut fn_count: usize = 0;
    let mut name_cache: HashMap<String, String> = HashMap::new();

    // Phase 1: type chunks (trait/impl/struct/enum)
    let mut seen_types = collect_type_chunks(&krate, &mut chunks);

    // Phase 2: function chunks + call edges
    // First pass: collect closure edges (closures are in all_local_items, not fn_defs)
    let mut closure_edges: HashMap<String, Vec<CallEdge>> = HashMap::new();
    for item in rustc_public::all_local_items() {
        let name_str = item.name().to_string();
        if !name_str.contains("{closure") || !item.has_body() { continue; }
        let body = item.body().unwrap();
        let parent_raw = &name_str[..name_str.find("::{closure").unwrap_or(name_str.len())];
        let parent_name = strip_crate_prefix(parent_raw);
        let parent_file = span_file(&body.span);
        let mut buf: Vec<CallEdge> = Vec::new();
        let mut extractor = CallExtractor {
            caller_name: parent_name.clone(),
            caller_file: parent_file,
            locals: body.locals(),
            edges: &mut buf,
            name_cache: &mut name_cache,
        };
        extractor.visit_body(&body);
        closure_edges.entry(parent_name).or_default().extend(buf);
    }

    // Second pass: regular functions
    for fn_def in krate.fn_defs() {
        let name_str = fn_def.name().to_string();
        if name_str.contains("{closure") { continue; }
        let item = rustc_public::CrateItem(fn_def.def_id());
        if !item.has_body() { continue; }
        fn_count += 1;
        let body = item.body().unwrap();
        let name = strip_crate_prefix(&name_str);
        let filename = span_file(&body.span);
        let (start_line, end_line) = span_lines(&body.span);

        // Source-based signature is extracted in ingest_mir from body text.
        // MIR fn_sig produces unreadable Ty debug output, so we skip it here.
        let signature: Option<String> = None;

        // Extract call edges
        let mut extractor = CallExtractor {
            caller_name: name.clone(),
            caller_file: filename.clone(),
            locals: body.locals(),
            edges: &mut edges,
            name_cache: &mut name_cache,
        };
        extractor.visit_body(&body);

        // Merge closure edges into parent function
        if let Some(ce) = closure_edges.remove(&name) {
            edges.extend(ce);
        }

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
    // Phase 3: use items + dependency mapping + accurate spans via HIR
    let (uses, use_deps, hir_spans) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(collect_use_deps))
        .unwrap_or_else(|e| { eprintln!("[mir-callgraph] HIR extraction failed: {e:?}"); (vec![], vec![], vec![]) });
    let span_map: HashMap<(&str, usize), (usize, usize)> = hir_spans.iter()
        .map(|(_, file, s, e)| ((file.as_str(), *s), (*s, *e))).collect();
    for c in &mut chunks {
        if c.start_line == c.end_line {
            if let Some(&(s, e)) = span_map.get(&(c.file.as_str(), c.start_line)) {
                c.start_line = s;
                c.end_line = e;
            }
        }
    }
    let t_mir = t_all.elapsed();
    let t_out = std::time::Instant::now();
    output::write_results(&crate_name, &edges, &chunks, &uses, &use_deps, fn_count, json, db_path);
    let t_db = t_out.elapsed();
    eprintln!("[prof:extract] {crate_name}: mir={:.0}us db={:.0}us fns={fn_count} chunks={}", t_mir.as_micros(), t_db.as_micros(), chunks.len());

    ControlFlow::Continue(())
}

fn collect_use_deps() -> (Vec<UseItem>, Vec<UseDep>, Vec<(String, String, usize, usize)>) {
    let mut uses = Vec::new();
    let mut deps = Vec::new();
    let mut hir_spans: Vec<(String, String, usize, usize)> = Vec::new();
    rustc_middle::ty::tls::with(|tcx| {
        let source_map = tcx.sess.source_map();
        let mut def_to_use: HashMap<rustc_hir::def_id::DefId, Vec<(String, usize)>> = HashMap::new();
        for item_id in tcx.hir_crate_items(()).free_items() {
            let item = tcx.hir_item(item_id);
            let rustc_hir::ItemKind::Use(path, kind) = &item.kind else { continue };
            let span = item.span;
            let file = match source_map.span_to_filename(span) {
                rustc_span::FileName::Real(ref name) => match name.local_path() {
                    Some(p) => format!("{}", p.display()).replace('\\', "/"),
                    None => continue,
                },
                _ => continue,
            };
            if !is_local_file(&file) { continue; }
            let line = source_map.lookup_char_pos(span.lo()).line;
            let source = source_map.span_to_snippet(span).unwrap_or_default();
            let kind_str = match kind {
                rustc_hir::UseKind::Single(_) => "single",
                rustc_hir::UseKind::Glob => "glob",
                rustc_hir::UseKind::ListStem => "list",
            };
            let resolved_path: String = path.segments.iter()
                .map(|seg| seg.ident.to_string()).collect::<Vec<_>>().join("::");
            uses.push(UseItem {
                file: file.clone(),
                line,
                source,
                resolved: format!("{resolved_path} ({kind_str})"),
            });
            if let Some(rustc_hir::def::Res::Def(_, def_id)) = path.res.type_ns {
                def_to_use.entry(def_id).or_default().push((file.clone(), line));
            }
            if let Some(rustc_hir::def::Res::Def(_, def_id)) = path.res.value_ns {
                def_to_use.entry(def_id).or_default().push((file, line));
            }
        }
        // Walk each function body to collect referenced DefIds
        for item_id in tcx.hir_crate_items(()).free_items() {
            let item = tcx.hir_item(item_id);
            let local_def_id = item.owner_id.def_id;
            let item_span = item.span;
            let fn_file = match source_map.span_to_filename(item_span) {
                rustc_span::FileName::Real(ref name) => match name.local_path() {
                    Some(p) => format!("{}", p.display()).replace('\\', "/"),
                    None => continue,
                },
                _ => continue,
            };
            if !is_local_file(&fn_file) { continue; }
            let fn_name = strip_crate_prefix(&tcx.def_path_str(local_def_id.to_def_id()));
            let start = source_map.lookup_char_pos(item_span.lo()).line;
            let end = source_map.lookup_char_pos(item_span.hi()).line;
            if start != end {
                hir_spans.push((fn_name.clone(), fn_file.clone(), start, end));
            }
            // Collect use deps for function bodies
            let has_body = matches!(item.kind, rustc_hir::ItemKind::Fn { .. } | rustc_hir::ItemKind::Static(..));
            if !has_body { continue; }
            let Ok(body) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tcx.hir_body_owned_by(local_def_id)
            })) else { continue };
            let mut collector = DefIdCollector { def_ids: Vec::new() };
            rustc_hir::intravisit::walk_body(&mut collector, &body);
            let mut seen_use_lines: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();
            for def_id in &collector.def_ids {
                if let Some(use_locs) = def_to_use.get(def_id) {
                    for (use_file, use_line) in use_locs {
                        if use_file == &fn_file && seen_use_lines.insert((use_file.clone(), *use_line)) {
                            deps.push(UseDep {
                                fn_name: fn_name.clone(),
                                fn_file: fn_file.clone(),
                                use_file: use_file.clone(),
                                use_line: *use_line,
                            });
                        }
                    }
                }
            }
        }
    });
    (uses, deps, hir_spans)
}

struct DefIdCollector {
    def_ids: Vec<rustc_hir::def_id::DefId>,
}

impl<'hir> rustc_hir::intravisit::Visitor<'hir> for DefIdCollector {
    fn visit_path(&mut self, path: &rustc_hir::Path<'hir>, _id: rustc_hir::HirId) {
        if let rustc_hir::def::Res::Def(_, def_id) = path.res {
            self.def_ids.push(def_id);
        }
        rustc_hir::intravisit::walk_path(self, path);
    }
    fn visit_expr(&mut self, expr: &'hir rustc_hir::Expr<'hir>) {
        if let rustc_hir::ExprKind::Path(ref qpath) = expr.kind {
            match qpath {
                rustc_hir::QPath::Resolved(_, path) => {
                    if let rustc_hir::def::Res::Def(_, def_id) = path.res {
                        self.def_ids.push(def_id);
                    }
                }
                _ => {}
            }
        }
        rustc_hir::intravisit::walk_expr(self, expr);
    }
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
