#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

use std::env;
use std::process::Command;

use rustc_middle::mir::TerminatorKind;
use rustc_middle::ty::TyCtxt;
use rustc_span::def_id::{DefId, LOCAL_CRATE};
use serde::{Deserialize, Serialize};

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
}

impl rustc_driver::Callbacks for MirCallbacks {
    fn after_analysis(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        tcx: TyCtxt<'_>,
    ) -> rustc_driver::Compilation {
        extract_all(tcx, self.json, self.is_test_target);
        rustc_driver::Compilation::Continue
    }
}

// ── Naming ─────────────────────────────────────────────────────────

/// Consistent name for a DefId.
///
/// Uses `def_path_str` (the standard rustc display name). For local items
/// this gives the raw definition path; for external items it may use
/// re-export visible paths, which edge_resolve handles via crate prefix
/// stripping and suffix matching.
fn canonical_name(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    tcx.def_path_str(def_id)
}

// ── Extraction ──────────────────────────────────────────────────────

fn extract_filename(source_map: &rustc_span::source_map::SourceMap, span: rustc_span::Span) -> String {
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

/// Extract visibility string from `tcx.visibility(def_id)`.
fn extract_visibility(tcx: TyCtxt<'_>, def_id: rustc_span::def_id::DefId) -> String {
    let vis = tcx.visibility(def_id);
    if vis.is_public() {
        "pub".to_string()
    } else {
        // Restricted visibility: pub(crate), pub(super), pub(in path), or private
        let vis_str = format!("{vis:?}");
        if vis_str.contains("Restricted") {
            // For pub(crate), the restricted DefId points to the crate root
            if let rustc_middle::ty::Visibility::Restricted(restricted_id) = vis {
                if restricted_id == tcx.parent_module_from_def_id(def_id.expect_local()).to_def_id() {
                    // private (restricted to own module) — empty string
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

fn extract_all(tcx: TyCtxt<'_>, json: bool, is_test_target: bool) {
    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let source_map = tcx.sess.source_map();

    let mut edges: Vec<CallEdge> = Vec::new();
    let mut chunks: Vec<MirChunk> = Vec::new();

    // ── Phase 1: HIR items — struct/enum/trait/impl ─────────────────
    for item_id in tcx.hir_free_items() {
        let item = tcx.hir_item(item_id);
        let kind_str = match &item.kind {
            rustc_hir::ItemKind::Struct(..) => "struct",
            rustc_hir::ItemKind::Enum(..) => "enum",
            rustc_hir::ItemKind::Trait(..) => "trait",
            rustc_hir::ItemKind::Impl(..) => "impl",
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
        });
    }

    // ── Phase 2: MIR keys — functions, closures (call edges + fn chunks) ──
    for &def_id in tcx.mir_keys(()) {
        let def_kind = tcx.def_kind(def_id);

        let is_fn = matches!(def_kind,
            rustc_hir::def::DefKind::Fn
            | rustc_hir::def::DefKind::AssocFn
        );
        let is_closure = matches!(def_kind, rustc_hir::def::DefKind::Closure);

        if !is_fn && !is_closure {
            // struct/enum/trait/impl are handled by HIR traversal above
            continue;
        }

        let body = tcx.optimized_mir(def_id);
        let caller_name = canonical_name(tcx, def_id.to_def_id());
        let caller_file = extract_filename(source_map, body.span);
        let caller_kind = match def_kind {
            rustc_hir::def::DefKind::Fn => "fn",
            rustc_hir::def::DefKind::AssocFn => "method",
            rustc_hir::def::DefKind::Closure => "closure",
            _ => "other",
        };

        // Emit chunk for functions (not closures)
        if is_fn {
            let start = source_map.lookup_char_pos(body.span.lo());
            let end = source_map.lookup_char_pos(body.span.hi());

            // fn signature: source text up to the first `{`
            let sig_str = source_map
                .span_to_snippet(body.span)
                .ok()
                .and_then(|snippet| {
                    snippet.find('{').map(|brace| snippet[..brace].trim().to_string())
                });

            let vis = extract_visibility(tcx, def_id.to_def_id());

            let is_test = is_test_target
                || caller_file.contains("/tests/")
                || caller_file.contains("\\tests\\")
                || caller_name.starts_with("test_")
                || caller_name.contains("::test_")
                || tcx.has_attr(def_id.to_def_id(), rustc_span::Symbol::intern("test"));

            let body_text = source_map.span_to_snippet(body.span).ok();
            chunks.push(MirChunk {
                name: caller_name.clone(),
                file: caller_file.clone(),
                kind: caller_kind.to_string(),
                start_line: start.line,
                end_line: end.line,
                signature: sig_str,
                visibility: vis,
                is_test,
                body: body_text.unwrap_or_default(),
            });
        }

        // Extract call edges + function reference edges
        let mut seen_refs: std::collections::HashSet<rustc_hir::def_id::DefId> = std::collections::HashSet::new();
        for block in body.basic_blocks.iter() {
            let terminator = block.terminator();

            // 1. Direct calls (TerminatorKind::Call)
            if let TerminatorKind::Call { ref func, ref args, .. } = terminator.kind {
                let func_ty = func.ty(&body.local_decls, tcx);
                let callee_def_id = match func_ty.kind() {
                    rustc_middle::ty::TyKind::FnDef(def_id, _) => *def_id,
                    _ => continue,
                };

                let callee_name = canonical_name(tcx, callee_def_id);
                let call_line = source_map
                    .lookup_char_pos(terminator.source_info.span.lo())
                    .line;

                let callee_span = tcx.def_span(callee_def_id);
                let callee_file_str = extract_filename(source_map, callee_span);
                let callee_start = source_map.lookup_char_pos(callee_span.lo()).line;

                edges.push(CallEdge {
                    caller: caller_name.clone(),
                    caller_file: caller_file.clone(),
                    caller_kind: caller_kind.to_string(),
                    callee: callee_name,
                    callee_file: callee_file_str,
                    callee_start_line: callee_start,
                    line: call_line,
                    is_local: callee_def_id.is_local(),
                });
                seen_refs.insert(callee_def_id);

                // 2. Function references passed as arguments
                for arg in args.iter() {
                    let arg_ty = arg.node.ty(&body.local_decls, tcx);
                    if let rustc_middle::ty::TyKind::FnDef(ref_def_id, _) = arg_ty.kind() {
                        if !seen_refs.contains(ref_def_id) {
                            seen_refs.insert(*ref_def_id);
                            let ref_name = canonical_name(tcx, *ref_def_id);
                            let ref_span = tcx.def_span(*ref_def_id);
                            let ref_file = extract_filename(source_map, ref_span);
                            let ref_start = source_map.lookup_char_pos(ref_span.lo()).line;
                            edges.push(CallEdge {
                                caller: caller_name.clone(),
                                caller_file: caller_file.clone(),
                                caller_kind: caller_kind.to_string(),
                                callee: ref_name,
                                callee_file: ref_file,
                                callee_start_line: ref_start,
                                line: call_line,
                                is_local: ref_def_id.is_local(),
                            });
                        }
                    }
                }
            }
        }
    }

    // ── Output ──────────────────────────────────────────────────────
    let out_dir = env::var("MIR_CALLGRAPH_OUT").ok();
    let db_path = env::var("MIR_CALLGRAPH_DB").ok();

    // Prefer sqlite direct write; fall back to JSONL for compatibility.
    if let Some(db) = &db_path {
        if let Ok(conn) = rusqlite::Connection::open(db) {
            let _ = conn.execute_batch("
                CREATE TABLE IF NOT EXISTS mir_edges (
                    caller TEXT, caller_file TEXT, caller_kind TEXT,
                    callee TEXT, callee_file TEXT, callee_start_line INTEGER,
                    line INTEGER, is_local INTEGER, crate_name TEXT
                );
                CREATE TABLE IF NOT EXISTS mir_chunks (
                    name TEXT, file TEXT, kind TEXT,
                    start_line INTEGER, end_line INTEGER,
                    signature TEXT, visibility TEXT, is_test INTEGER,
                    body TEXT, crate_name TEXT
                );
            ");

            if let Ok(tx) = conn.unchecked_transaction() {
                {
                    let mut edge_stmt = tx.prepare_cached(
                        "INSERT INTO mir_edges VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"
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
                        "INSERT INTO mir_chunks VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"
                    ).unwrap();
                    for c in &chunks {
                        let _ = chunk_stmt.execute(rusqlite::params![
                            c.name, c.file, c.kind,
                            c.start_line, c.end_line,
                            c.signature, c.visibility, c.is_test as i32,
                            c.body, crate_name,
                        ]);
                    }
                }
                let _ = tx.commit();
            }
        }
        eprintln!(
            "[mir-callgraph] {crate_name}: {} edges, {} chunks (sqlite)",
            edges.len(), chunks.len()
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
            "[mir-callgraph] {crate_name}: {} edges, {} chunks",
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
            let mut callbacks = MirCallbacks { json, is_test_target };
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
            // This ensures env!() macros and proc macros work identically to cargo mode.
            for (key, value) in &cached.env {
                // SAFETY: single-threaded at this point (before rustc_driver).
                unsafe { env::set_var(key, value); }
            }

            let is_test_target = cached.args.iter().any(|a| a == "--test");
            let mut callbacks = MirCallbacks { json, is_test_target };
            rustc_driver::run_compiler(&cached.args, &mut callbacks);
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
