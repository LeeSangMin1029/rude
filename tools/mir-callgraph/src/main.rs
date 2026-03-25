#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

// Inline compat module — mir-callgraph is embedded as single file via include_str!
mod compat {
    pub use rustc_hir::def::DefKind;
    pub use rustc_hir::ItemKind;
    pub use rustc_middle::mir::TerminatorKind;
    pub use rustc_middle::ty::TyCtxt;
    pub use rustc_span::def_id::{DefId, LOCAL_CRATE};

    pub fn canonical_name(tcx: TyCtxt<'_>, def_id: DefId) -> String {
        tcx.def_path_str(def_id)
    }

    pub fn extract_filename(
        source_map: &rustc_span::source_map::SourceMap,
        span: rustc_span::Span,
    ) -> String {
        match source_map.span_to_filename(span) {
            rustc_span::FileName::Real(ref name) => {
                let path_str = format!("{name:?}");
                if let Some(start) = path_str.find("name: \"") {
                    let rest = &path_str[start + 7..];
                    if let Some(end) = rest.find('"') {
                        return rest[..end].replace("\\\\", "/").to_string();
                    }
                }
                path_str
            }
            other => format!("{other:?}"),
        }
    }

    pub fn extract_visibility(tcx: TyCtxt<'_>, def_id: DefId) -> String {
        let vis = tcx.visibility(def_id);
        if vis.is_public() {
            "pub".to_string()
        } else {
            let vis_str = format!("{vis:?}");
            if vis_str.contains("Restricted") {
                if let rustc_middle::ty::Visibility::Restricted(restricted_id) = vis {
                    if restricted_id == tcx.parent_module_from_def_id(def_id.expect_local()).to_def_id() {
                        String::new()
                    } else if restricted_id == LOCAL_CRATE.as_def_id() {
                        "pub(crate)".to_string()
                    } else {
                        format!("pub(in {})", tcx.def_path_str(restricted_id))
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        }
    }
}

use std::env;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::compat::{
    canonical_name, extract_filename, extract_visibility, DefId, DefKind, ItemKind,
    TerminatorKind, TyCtxt, LOCAL_CRATE,
};

// ── Output types ────────────────────────────────────────────────────

#[derive(Serialize)]
struct CallEdge {
    caller: String,
    caller_file: String,
    caller_kind: String,
    callee: String,
    callee_file: String,
    callee_start_line: usize,
    line: usize,
    is_local: bool,
}

/// Chunk definition extracted from MIR — replaces RA file_structure + source parsing.
#[derive(Serialize)]
struct MirChunk {
    name: String,
    file: String,
    kind: String,
    start_line: usize,
    end_line: usize,
    signature: Option<String>,
    visibility: String,
    is_test: bool,
    /// Full source text of the item.
    body: String,
    /// Comma-separated callee names with call lines: "name@line,name@line,..."
    #[serde(default)]
    calls: String,
    /// Comma-separated type references.
    #[serde(default)]
    type_refs: String,
}

// ── Cached rustc args for direct mode ───────────────────────────────

#[derive(Serialize, Deserialize)]
struct RustcArgs {
    args: Vec<String>,
    crate_name: String,
    sysroot: String,
    /// Environment variables set by cargo/build.rs (CARGO_*, DEP_*, OUT_DIR, custom).
    #[serde(default)]
    env: Vec<(String, String)>,
}

// ── Callbacks ───────────────────────────────────────────────────────

struct MirCallbacks {
    json: bool,
    /// True if compiling with --test flag (all functions are test-related).
    is_test_target: bool,
    /// Optional sqlite DB path for direct output.
    db_path: Option<String>,
}

impl rustc_driver::Callbacks for MirCallbacks {
    fn after_analysis(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        tcx: TyCtxt<'_>,
    ) -> rustc_driver::Compilation {
        extract_all(tcx, self.json, self.is_test_target, &self.db_path);
        rustc_driver::Compilation::Continue
    }
}

// ── Extraction ──────────────────────────────────────────────────────

fn extract_all(tcx: TyCtxt<'_>, json: bool, is_test_target: bool, db_path: &Option<String>) {
    let _span = tracing::info_span!("extract_all").entered();
    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let source_map = tcx.sess.source_map();

    let mut edges: Vec<CallEdge> = Vec::new();
    let mut chunks: Vec<MirChunk> = Vec::new();

    // ── Phase 1: HIR items — struct/enum/trait/impl ─────────────────
    {let _hir = tracing::info_span!("hir_items").entered();
    for item_id in tcx.hir_free_items() {
        let item = tcx.hir_item(item_id);
        let kind_str = match &item.kind {
            ItemKind::Struct(..) => "struct",
            ItemKind::Enum(..) => "enum",
            ItemKind::Trait(..) => "trait",
            ItemKind::Impl(..) => "impl",
            _ => continue,
        };

        let span = item.span;
        let file = extract_filename(source_map, span);
        let start = source_map.lookup_char_pos(span.lo());
        let end = source_map.lookup_char_pos(span.hi());
        let def_id = item.owner_id.def_id;
        let vis = extract_visibility(tcx, def_id.to_def_id());

        let name = canonical_name(tcx, def_id.to_def_id());

        // Signature from source: everything before the first `{`
        let sig_str = source_map
            .span_to_snippet(span)
            .ok()
            .and_then(|snippet| {
                snippet.find('{').map(|brace| snippet[..brace].trim().to_string())
            });

        let body_text = source_map.span_to_snippet(span).ok();
        chunks.push(MirChunk {
            name,
            file,
            kind: kind_str.to_string(),
            start_line: start.line,
            end_line: end.line,
            signature: sig_str,
            visibility: vis,
            is_test: false,
            body: body_text.unwrap_or_default(),
            calls: String::new(),
            type_refs: String::new(),
        });
    }

    } // end hir_items span

    // ── Pre-build trait method → local impls cache ──────────────────
    // Key: (trait_def_id, method_name) → Vec<(callee_name, callee_file, callee_start_line)>
    // Avoids repeated tcx.trait_impls_of() + associated_items() per call site (23x speedup).
    // Cache keyed by callee DefId (the trait method). callee_def_id is unique per
    // (trait, method) so no need for a compound key.
    let mut trait_impl_cache: std::collections::HashMap<
        DefId,
        Vec<(String, String, usize)>,
    > = std::collections::HashMap::new();

    // ── Phase 2: MIR keys — functions, closures (call edges + fn chunks) ──
    let _mir_phase = tracing::info_span!("mir_extraction").entered();
    let mut fn_count: usize = 0;
    let mut name_cache: std::collections::HashMap<DefId, String> = std::collections::HashMap::new();

    let mir_keys = { let _s = tracing::info_span!("tcx_mir_keys").entered(); tcx.mir_keys(()) };
    for &def_id in mir_keys {
        let def_kind = tcx.def_kind(def_id);
        let is_fn = matches!(def_kind, DefKind::Fn | DefKind::AssocFn);
        let is_closure = matches!(def_kind, DefKind::Closure);
        if !is_fn && !is_closure { continue; }

        let _fn_span = tracing::info_span!("per_function").entered();

        let body = { let _s = tracing::info_span!("optimized_mir").entered(); tcx.optimized_mir(def_id) };
        let caller_name = { let _s = tracing::info_span!("canonical_name_caller").entered(); canonical_name(tcx, def_id.to_def_id()) };
        fn_count += 1;
        let caller_file = { let _s = tracing::info_span!("extract_filename_caller").entered(); extract_filename(source_map, body.span) };
        let caller_kind = match def_kind {
            DefKind::Fn => "fn", DefKind::AssocFn => "method", DefKind::Closure => "closure", _ => "other",
        };

        if is_fn {
            let _s = tracing::info_span!("chunk_build").entered();
            let start = source_map.lookup_char_pos(body.span.lo());
            let end = source_map.lookup_char_pos(body.span.hi());
            let sig_str = { let _s2 = tracing::info_span!("span_to_snippet_sig").entered();
                source_map.span_to_snippet(body.span).ok()
                    .and_then(|snippet| snippet.find('{').map(|brace| snippet[..brace].trim().to_string()))
            };
            let vis = { let _s2 = tracing::info_span!("extract_visibility").entered(); extract_visibility(tcx, def_id.to_def_id()) };
            let is_test = { let _s2 = tracing::info_span!("is_test_check").entered();
                is_test_target
                || caller_file.contains("/tests/") || caller_file.contains("\\tests\\")
                || caller_name.starts_with("test_") || caller_name.contains("::test_")
                || { let hir_id = tcx.local_def_id_to_hir_id(def_id);
                     let attrs = tcx.hir_attrs(hir_id);
                     attrs.iter().any(|a| a.has_name(rustc_span::Symbol::intern("test"))) }
            };
            let body_text = { let _s2 = tracing::info_span!("span_to_snippet_body").entered();
                source_map.span_to_snippet(body.span).ok()
            };
            chunks.push(MirChunk {
                name: caller_name.clone(), file: caller_file.clone(), kind: caller_kind.to_string(),
                start_line: start.line, end_line: end.line, signature: sig_str, visibility: vis,
                is_test, body: body_text.unwrap_or_default(), calls: String::new(), type_refs: String::new(),
            });
        }

        // Extract call edges
        let _edge_span = tracing::info_span!("edge_extraction").entered();
        let mut seen_refs: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        for block in body.basic_blocks.iter() {
            let terminator = block.terminator();
            if let TerminatorKind::Call { ref func, ref args, .. } = terminator.kind {
                let func_ty = func.ty(&body.local_decls, tcx);
                let callee_def_id = match func_ty.kind() {
                    rustc_middle::ty::TyKind::FnDef(def_id, _) => *def_id,
                    _ => continue,
                };

                let callee_name = { let _s = tracing::info_span!("canonical_name_callee").entered();
                    name_cache.entry(callee_def_id).or_insert_with(|| canonical_name(tcx, callee_def_id)).clone()
                };
                let call_line = { let _s = tracing::info_span!("lookup_call_line").entered();
                    source_map.lookup_char_pos(terminator.source_info.span.lo()).line
                };
                let is_trait_method = { let _s = tracing::info_span!("def_kind_check").entered();
                    matches!(tcx.def_kind(callee_def_id), DefKind::AssocFn)
                    && matches!(tcx.def_kind(tcx.parent(callee_def_id)), DefKind::Trait)
                };

                if is_trait_method {
                    let _s = tracing::info_span!("trait_edge_push").entered();
                    edges.push(CallEdge {
                        caller: caller_name.clone(), caller_file: caller_file.clone(),
                        caller_kind: caller_kind.to_string(), callee: callee_name,
                        callee_file: String::new(), callee_start_line: 0,
                        line: call_line, is_local: callee_def_id.is_local(),
                    });
                } else {
                    let _s = tracing::info_span!("direct_edge_push").entered();
                    let (callee_file_str, callee_start) = if callee_def_id.is_local() {
                        let callee_span = tcx.def_span(callee_def_id);
                        (extract_filename(source_map, callee_span),
                         source_map.lookup_char_pos(callee_span.lo()).line)
                    } else { (String::new(), 0) };
                    edges.push(CallEdge {
                        caller: caller_name.clone(), caller_file: caller_file.clone(),
                        caller_kind: caller_kind.to_string(), callee: callee_name,
                        callee_file: callee_file_str, callee_start_line: callee_start,
                        line: call_line, is_local: callee_def_id.is_local(),
                    });
                }
                seen_refs.insert(callee_def_id);

                // Function references passed as arguments
                for arg in args.iter() {
                    let arg_ty = arg.node.ty(&body.local_decls, tcx);
                    if let rustc_middle::ty::TyKind::FnDef(ref_def_id, _) = arg_ty.kind() {
                        if !seen_refs.contains(ref_def_id) {
                            seen_refs.insert(*ref_def_id);
                            let ref_name = name_cache.entry(*ref_def_id)
                                .or_insert_with(|| canonical_name(tcx, *ref_def_id)).clone();
                            let (ref_file, ref_start) = if ref_def_id.is_local() {
                                let ref_span = tcx.def_span(*ref_def_id);
                                (extract_filename(source_map, ref_span),
                                 source_map.lookup_char_pos(ref_span.lo()).line)
                            } else { (String::new(), 0) };
                            edges.push(CallEdge {
                                caller: caller_name.clone(), caller_file: caller_file.clone(),
                                caller_kind: caller_kind.to_string(), callee: ref_name,
                                callee_file: ref_file, callee_start_line: ref_start,
                                line: call_line, is_local: ref_def_id.is_local(),
                            });
                        }
                    }
                }
            }
        }
    }

    // Fill calls for all function chunks at once
    { let _s = tracing::info_span!("fill_calls").entered();
        let mut calls_by_caller: std::collections::HashMap<&str, Vec<String>> = std::collections::HashMap::new();
        for e in &edges {
            calls_by_caller.entry(&e.caller).or_default().push(format!("{}@{}", e.callee, e.line));
        }
        for c in &mut chunks {
            if c.kind == "fn" || c.kind == "method" {
                if let Some(fn_calls) = calls_by_caller.remove(c.name.as_str()) { c.calls = fn_calls.join(", "); }
            }
        }
    }

    drop(_mir_phase);

    // ── Output ──────────────────────────────────────────────────────
    let _sql = tracing::info_span!("sqlite_write").entered();
    let out_dir = env::var("MIR_CALLGRAPH_OUT").ok();

    // Prefer sqlite direct write; fall back to JSONL for compatibility.
    if let Some(db) = &db_path {
        if let Ok(conn) = rusqlite::Connection::open(db) {
            // WAL mode + busy timeout for safe concurrent writes (lib + test targets).
            let _ = conn.pragma_update(None, "journal_mode", "wal");
            conn.busy_timeout(std::time::Duration::from_secs(30)).ok();

            let _ = conn.execute_batch("
                CREATE TABLE IF NOT EXISTS mir_edges (
                    caller TEXT, caller_file TEXT, caller_kind TEXT,
                    callee TEXT, callee_file TEXT, callee_start_line INTEGER,
                    line INTEGER, is_local INTEGER, crate_name TEXT,
                    UNIQUE(caller, callee, line, crate_name)
                );
                CREATE TABLE IF NOT EXISTS mir_chunks (
                    name TEXT, file TEXT, kind TEXT,
                    start_line INTEGER, end_line INTEGER,
                    signature TEXT, visibility TEXT, is_test INTEGER,
                    body TEXT, calls TEXT, type_refs TEXT, crate_name TEXT,
                    UNIQUE(name, kind, crate_name)
                );
            ");

            if let Ok(tx) = conn.unchecked_transaction() {
                // INSERT OR IGNORE only — caller (rude add) clears tables before build.
                // This allows lib + test compilations to safely accumulate edges.
                {
                    let mut edge_stmt = tx.prepare_cached(
                        "INSERT OR IGNORE INTO mir_edges VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"
                    ).unwrap();
                    for e in &edges {
                        let _ = edge_stmt.execute(rusqlite::params![
                            e.caller, e.caller_file, e.caller_kind,
                            e.callee, e.callee_file, e.callee_start_line,
                            e.line, e.is_local as i32, crate_name,
                        ]);
                    }
                }
                {
                    let mut chunk_stmt = tx.prepare_cached(
                        "INSERT OR IGNORE INTO mir_chunks VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"
                    ).unwrap();
                    for c in &chunks {
                        let _ = chunk_stmt.execute(rusqlite::params![
                            c.name, c.file, c.kind,
                            c.start_line, c.end_line,
                            c.signature, c.visibility, c.is_test as i32,
                            "", c.calls, c.type_refs, crate_name,
                        ]);
                    }
                }
                let _ = tx.commit();
            }
        }
        eprintln!(
            "[mir-callgraph] {crate_name}: {} edges, {} chunks ({fn_count} fns)",
            edges.len(), chunks.len(),
        );
    } else if let Some(dir) = &out_dir {
        use std::io::Write;

        let edge_path = format!("{dir}/{crate_name}.edges.jsonl");
        if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&edge_path) {
            let mut w = std::io::BufWriter::new(file);
            for edge in &edges {
                if let Ok(s) = serde_json::to_string(edge) {
                    let _ = writeln!(w, "{s}");
                }
            }
        }

        let chunk_path = format!("{dir}/{crate_name}.chunks.jsonl");
        if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&chunk_path) {
            let mut w = std::io::BufWriter::new(file);
            for chunk in &chunks {
                if let Ok(s) = serde_json::to_string(chunk) {
                    let _ = writeln!(w, "{s}");
                }
            }
        }

        eprintln!(
            "[mir-callgraph] {crate_name}: {} edges, {} chunks ({fn_count} fns)",
            edges.len(), chunks.len()
        );
    } else if json {
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut w = std::io::BufWriter::new(stdout.lock());
        for edge in &edges {
            if let Ok(s) = serde_json::to_string(edge) {
                let _ = writeln!(w, "{s}");
            }
        }
    } else {
        eprintln!(
            "[mir-callgraph] {crate_name}: {} edges, {} chunks",
            edges.len(), chunks.len()
        );
    }
}

// ── Main ────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();

    // Mode 1: RUSTC_WRAPPER mode
    let is_wrapper = args.get(1).is_some_and(|a| a.contains("rustc") && !a.starts_with("-"));

    if is_wrapper {
        let rustc_args: Vec<String> = args[2..].to_vec();

        if env::var("MIR_CALLGRAPH_DEBUG").is_ok() {
            eprintln!("[mir-cg] wrapper args: {:?}", &rustc_args);
        }

        let is_local = rustc_args.iter().any(|a| {
            a.ends_with(".rs")
                && !a.contains(".cargo")
                && !a.contains("registry")
                && !a.contains("rustup")
        });
        let has_edition = rustc_args.iter().any(|a| a.starts_with("--edition"));
        let is_build_script = rustc_args.iter().any(|a| a == "build_script_build" || a.contains("build.rs"));

        if has_edition && is_local && !is_build_script {
            let mut full_args = vec![args[1].clone()];
            full_args.extend(rustc_args.iter().cloned());

            // Enable parallel frontend for faster type checking
            full_args.push("-Z".to_string());
            full_args.push("threads=8".to_string());

            if !full_args.iter().any(|a| a.starts_with("--sysroot")) {
                if let Ok(output) = Command::new(&args[1]).arg("--print").arg("sysroot").output() {
                    let sysroot = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !sysroot.is_empty() {
                        full_args.push("--sysroot".to_string());
                        full_args.push(sysroot);
                    }
                }
            }

            // Cache rustc args for direct mode (bypass cargo on subsequent runs)
            if let Ok(out_dir) = env::var("MIR_CALLGRAPH_OUT") {
                let crate_name = rustc_args.iter()
                    .position(|a| a == "--crate-name")
                    .and_then(|i| rustc_args.get(i + 1))
                    .cloned()
                    .unwrap_or_default();
                let sysroot = full_args.iter()
                    .position(|a| a == "--sysroot")
                    .and_then(|i| full_args.get(i + 1))
                    .cloned()
                    .unwrap_or_default();
                if !crate_name.is_empty() {
                    let args_dir = format!("{out_dir}/rustc-args");
                    let _ = std::fs::create_dir_all(&args_dir);
                    // Snapshot env vars from cargo/build.rs for direct mode.
                    // Strategy: save ALL env vars at RUSTC_WRAPPER invocation time,
                    // then in direct mode restore them. This guarantees build.rs
                    // custom env vars (via `cargo::rustc-env=K=V`) are preserved
                    // without needing to know their names in advance.
                    //
                    // We exclude only a few large/irrelevant vars to keep the
                    // JSON size reasonable (~5KB instead of ~20KB).
                    let env_snapshot: Vec<(String, String)> = env::vars()
                        .filter(|(k, _)| {
                            !matches!(k.as_str(),
                                "PATH" | "PSModulePath" | "PATHEXT" | "CARGO_MAKEFLAGS"
                            ) && !k.starts_with("MIR_CALLGRAPH_")
                        })
                        .collect();
                    let cached = RustcArgs {
                        args: full_args.clone(),
                        crate_name: crate_name.clone(),
                        sysroot,
                        env: env_snapshot,
                    };
                    // Save per target: lib and test get separate files
                    let is_test = rustc_args.iter().any(|a| a == "--test");
                    let suffix = if is_test { ".test" } else { ".lib" };
                    if let Ok(json_str) = serde_json::to_string_pretty(&cached) {
                        let path = format!("{args_dir}/{crate_name}{suffix}.rustc-args.json");
                        let _ = std::fs::write(&path, json_str);
                    }
                }
            }

            let json = env::var("MIR_CALLGRAPH_JSON").is_ok();
            let is_test_target = rustc_args.iter().any(|a| a == "--test");
            let db_path = env::var("MIR_CALLGRAPH_DB").ok();
            let mut callbacks = MirCallbacks { json, is_test_target, db_path };
            rustc_driver::run_compiler(&full_args, &mut callbacks);
        } else {
            let status = Command::new(&args[1])
                .args(&args[2..])
                .status()
                .expect("failed to run rustc");
            std::process::exit(status.code().unwrap_or(1));
        }
        return;
    }

    // Mode 2: Direct mode — use cached rustc args, bypass cargo entirely
    if args.iter().any(|a| a == "--direct") {
        let args_files: Vec<&String> = args.iter()
            .skip_while(|a| *a != "--args-file")
            .skip(1)
            .take_while(|a| !a.starts_with("--"))
            .collect();

        if args_files.is_empty() {
            eprintln!("[mir-callgraph] --direct requires at least one --args-file <path>");
            std::process::exit(1);
        }

        let json = env::var("MIR_CALLGRAPH_JSON").is_ok();
        let mut had_error = false;

        for args_file in &args_files {
            let content = match std::fs::read_to_string(args_file) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[mir-callgraph] failed to read {args_file}: {e}");
                    had_error = true;
                    continue;
                }
            };
            let cached: RustcArgs = match serde_json::from_str(&content) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[mir-callgraph] failed to parse {args_file}: {e}");
                    had_error = true;
                    continue;
                }
            };

            eprintln!("[mir-callgraph] direct: compiling crate '{}'", cached.crate_name);

            // Restore env vars captured from cargo/build.rs during RUSTC_WRAPPER run.
            for (key, value) in &cached.env {
                // SAFETY: single-threaded at this point (before rustc_driver).
                unsafe { env::set_var(key, value); }
            }

            let is_test_target = cached.args.iter().any(|a| a == "--test");
            let db_path = env::var("MIR_CALLGRAPH_DB").ok();
            let mut callbacks = MirCallbacks { json, is_test_target, db_path };

            // Tracing: write chrome trace to profile dir
            let _guard = if env::var("MIR_PROFILE").is_ok() {
                use tracing_subscriber::prelude::*;
                let trace_file = format!("D:/rude/profile/{}.trace.json", cached.crate_name);
                let (chrome_layer, guard) = tracing_chrome::ChromeLayerBuilder::new()
                    .file(trace_file)
                    .include_args(true)
                    .build();
                tracing_subscriber::registry().with(chrome_layer).init();
                Some(guard)
            } else {
                None
            };

            { let _s = tracing::info_span!("run_compiler").entered();
              rustc_driver::run_compiler(&cached.args, &mut callbacks); }
        }

        if had_error {
            std::process::exit(1);
        }
        return;
    }

    // Mode 3: CLI mode (cargo wrapper)
    let json = args.iter().any(|a| a == "--json");
    let exe = env::current_exe().unwrap_or_default();

    let keep_going = args.iter().any(|a| a == "--keep-going");
    let mut cmd = Command::new("cargo");
    cmd.arg("+nightly")
        .arg("check")
        .arg("--tests")
        // Use a separate target dir so cargo doesn't skip crates already
        // compiled by a normal `cargo build` (different RUSTC_WRAPPER fingerprint).
        .arg("--target-dir").arg("target/mir-check")
        .env("RUSTC_WRAPPER", &exe);
    if keep_going {
        cmd.arg("--keep-going");
    }

    if json {
        cmd.env("MIR_CALLGRAPH_JSON", "1");
    }

    for arg in args.iter().skip(1).filter(|a| *a != "--json" && *a != "--keep-going") {
        cmd.arg(arg);
    }

    let status = cmd.status().expect("failed to run cargo check");
    std::process::exit(status.code().unwrap_or(1));
}

