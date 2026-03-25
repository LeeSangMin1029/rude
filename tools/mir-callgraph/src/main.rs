#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_public;

use std::env;
use std::ops::ControlFlow;
use std::process::Command;

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
    body: String,
    #[serde(default)]
    calls: String,
    #[serde(default)]
    type_refs: String,
}

// ── Cached rustc args for direct mode ───────────────────────────────

#[derive(Serialize, Deserialize)]
struct RustcArgs {
    args: Vec<String>,
    crate_name: String,
    sysroot: String,
    #[serde(default)]
    env: Vec<(String, String)>,
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Strip crate prefix: "rude_intel::context_cmd::build_context" → "context_cmd::build_context"
/// Preserves `<` for impl names: "<rude_db::db::StorageEngine as ...>" → "<db::StorageEngine as ...>"
fn strip_crate_prefix(name: &str) -> String {
    if let Some(inner) = name.strip_prefix('<') {
        // "<crate::Type as crate::Trait>" → "<Type as Trait>" (strip both prefixes)
        let stripped = if let Some(pos) = inner.find("::") {
            &inner[pos + 2..]
        } else { inner };
        format!("<{stripped}")
    } else if let Some(pos) = name.find("::") {
        name[pos + 2..].to_string()
    } else {
        name.to_string()
    }
}

fn span_file(span: &rustc_public::ty::Span) -> String {
    let f = span.get_filename().to_string();
    f.replace('\\', "/")
}

fn span_lines(span: &rustc_public::ty::Span) -> (usize, usize) {
    let info = span.get_lines();
    (info.start_line, info.end_line)
}

// ── Extraction ──────────────────────────────────────────────────────

fn extract_all(is_test_target: bool, json: bool, db_path: &Option<String>) -> ControlFlow<()> {
    use rustc_public::CrateDef;
    use rustc_public::mir::{MirVisitor, Terminator, TerminatorKind};
    use rustc_public::mir::visit::Location;
    use rustc_public::ty::{RigidTy, TyKind};


    let _span = tracing::info_span!("extract_all").entered();
    let krate = rustc_public::local_crate();
    let crate_name = krate.name.to_string();

    let mut edges: Vec<CallEdge> = Vec::new();
    let mut chunks: Vec<MirChunk> = Vec::new();
    let mut fn_count: usize = 0;
    let mut name_cache: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    // ── Phase 1: trait decls + trait impls → struct/enum/trait/impl chunks ──
    {
        let _phase = tracing::info_span!("trait_decl_impl_chunks").entered();

        // Trait declarations → trait chunks
        for t in rustc_public::all_trait_decls() {
            let file = span_file(&t.span());
            // Skip external traits (absolute paths)
            if file.starts_with('/') || file.contains(":/") { continue; }
            let (start, end) = span_lines(&t.span());
            chunks.push(MirChunk {
                name: strip_crate_prefix(&t.name().to_string()),
                file, kind: "trait".to_string(),
                start_line: start, end_line: end,
                signature: None, visibility: String::new(),
                is_test: false, body: String::new(),
                calls: String::new(), type_refs: String::new(),
            });
        }

        // Trait impls → impl chunks (+ infer struct/enum from self type name)
        let mut seen_types: std::collections::HashSet<String> = std::collections::HashSet::new();
        for i in rustc_public::all_trait_impls() {
            let file = span_file(&i.span());
            if file.starts_with('/') || file.contains(":/") { continue; }
            let (start, end) = span_lines(&i.span());
            let impl_name = strip_crate_prefix(&i.name().to_string());
            chunks.push(MirChunk {
                name: impl_name.clone(), file: file.clone(),
                kind: "impl".to_string(),
                start_line: start, end_line: end,
                signature: None, visibility: String::new(),
                is_test: false, body: String::new(),
                calls: String::new(), type_refs: String::new(),
            });

            // Extract struct/enum name from impl name: "<Foo as Bar>" → "Foo"
            if let Some(rest) = impl_name.strip_prefix('<') {
                if let Some(type_name) = rest.split(" as ").next() {
                    let type_name = type_name.trim().to_string();
                    if !type_name.is_empty() && seen_types.insert(type_name.clone()) {
                        // We don't know if it's struct or enum — use "struct" as default
                        chunks.push(MirChunk {
                            name: type_name, file: file.clone(),
                            kind: "struct".to_string(),
                            start_line: start, end_line: end,
                            signature: None, visibility: String::new(),
                            is_test: false, body: String::new(),
                            calls: String::new(), type_refs: String::new(),
                        });
                    }
                }
            }
        }
    }

    // ── Phase 2: functions → fn chunks + call edges ─────────────────
    let items = rustc_public::all_local_items();

    for item in &items {
        let raw_name = item.name().to_string();
        let kind = item.kind();
        let kind_debug = format!("{kind:?}");
        // Only process functions with MIR body
        if kind_debug != "Fn" || raw_name.contains("{closure") || !item.has_body() { continue; }
        fn_count += 1;

        let name = strip_crate_prefix(&raw_name);

        let body = item.body().unwrap();
        let filename = span_file(&body.span);
        let (start_line, end_line) = span_lines(&body.span);

        {
            let caller_file = filename.clone();
            let caller_name = name.clone();

        struct CallExtractor<'a> {
            caller_name: String,
            caller_file: String,
            edges: &'a mut Vec<CallEdge>,
            name_cache: &'a mut std::collections::HashMap<String, String>,
        }

        impl MirVisitor for CallExtractor<'_> {
            fn visit_terminator(&mut self, term: &Terminator, _loc: Location) {
                if let TerminatorKind::Call { ref func, .. } = term.kind {
                    if let Ok(op_ty) = func.ty(&[]) {
                        if let TyKind::RigidTy(RigidTy::FnDef(def, _args)) = op_ty.kind() {
                            use rustc_public::CrateDef as _;
                            let raw_callee = def.name().to_string();
                            let callee_name = self.name_cache.entry(raw_callee.clone())
                                .or_insert_with(|| strip_crate_prefix(&raw_callee))
                                .clone();

                            let callee_span = def.span();
                            let callee_file = span_file(&callee_span);
                            let (callee_start, _) = span_lines(&callee_span);

                            let call_span = term.span;
                            let (call_line, _) = span_lines(&call_span);

                            let is_external = callee_file.starts_with('/') || callee_file.contains(":/");
                            let (cf, cs) = if is_external {
                                (String::new(), 0)
                            } else {
                                (callee_file, callee_start)
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
                    // Also capture function references passed as arguments
                    if let TerminatorKind::Call { ref args, .. } = term.kind {
                        for arg in args {
                            if let Ok(arg_ty) = arg.ty(&[]) {
                                if let TyKind::RigidTy(RigidTy::FnDef(ref_def, _)) = arg_ty.kind() {
                                    use rustc_public::CrateDef as _;
                                    let ref_name = self.name_cache.entry(ref_def.name().to_string())
                                        .or_insert_with(|| strip_crate_prefix(&ref_def.name().to_string())).clone();
                                    let ref_span = ref_def.span();
                                    let ref_file = span_file(&ref_span);
                                    let (ref_start, _) = span_lines(&ref_span);
                                    let is_ext = ref_file.starts_with('/') || ref_file.contains(":/");
                                    let (rf, rs) = if is_ext { (String::new(), 0) } else { (ref_file, ref_start) };
                                    let (cl, _) = span_lines(&term.span);
                                    self.edges.push(CallEdge {
                                        caller: self.caller_name.clone(), caller_file: self.caller_file.clone(),
                                        caller_kind: "fn".to_string(), callee: ref_name,
                                        callee_file: rf, callee_start_line: rs,
                                        line: cl, is_local: !is_ext,
                                    });
                                }
                            }
                        }
                    }
                }
                self.super_terminator(term, _loc);
            }
        }

        let mut extractor = CallExtractor {
            caller_name: caller_name.clone(),
            caller_file: caller_file.clone(),
            edges: &mut edges,
            name_cache: &mut name_cache,
        };
            extractor.visit_body(&body);
        }

        // Build chunk for all items
        let is_test = is_test_target
            || filename.contains("/tests/") || filename.contains("\\tests\\")
            || name.starts_with("test_") || name.contains("::test_");

        chunks.push(MirChunk {
            name,
            file: filename,
            kind: "fn".to_string(),
            start_line,
            end_line,
            signature: None,
            visibility: String::new(),
            is_test,
            body: String::new(),
            calls: String::new(),
            type_refs: String::new(),
        });
    }

    // Fill calls per function chunk
    {
        let mut calls_by_caller: std::collections::HashMap<&str, Vec<String>> =
            std::collections::HashMap::new();
        for e in &edges {
            calls_by_caller.entry(&e.caller)
                .or_default()
                .push(format!("{}@{}", e.callee, e.line));
        }
        for c in &mut chunks {
            if let Some(fn_calls) = calls_by_caller.remove(c.name.as_str()) {
                c.calls = fn_calls.join(", ");
            }
        }
    }

    // ── Output ──────────────────────────────────────────────────────
    let out_dir = env::var("MIR_CALLGRAPH_OUT").ok();

    if let Some(db) = db_path {
        if let Ok(conn) = rusqlite::Connection::open(db) {
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
                {
                    let mut stmt = tx.prepare_cached(
                        "INSERT OR IGNORE INTO mir_edges VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"
                    ).unwrap();
                    for e in &edges {
                        let _ = stmt.execute(rusqlite::params![
                            e.caller, e.caller_file, e.caller_kind,
                            e.callee, e.callee_file, e.callee_start_line,
                            e.line, e.is_local as i32, crate_name,
                        ]);
                    }
                }
                {
                    let mut stmt = tx.prepare_cached(
                        "INSERT OR IGNORE INTO mir_chunks VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"
                    ).unwrap();
                    for c in &chunks {
                        let _ = stmt.execute(rusqlite::params![
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
        eprintln!("[mir-callgraph] {crate_name}: {} edges, {} chunks ({fn_count} fns)", edges.len(), chunks.len());
    } else if let Some(dir) = &out_dir {
        use std::io::Write;
        let p = format!("{dir}/{crate_name}.edges.jsonl");
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            let mut w = std::io::BufWriter::new(f);
            for e in &edges { if let Ok(s) = serde_json::to_string(e) { let _ = writeln!(w, "{s}"); } }
        }
        let p = format!("{dir}/{crate_name}.chunks.jsonl");
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            let mut w = std::io::BufWriter::new(f);
            for c in &chunks { if let Ok(s) = serde_json::to_string(c) { let _ = writeln!(w, "{s}"); } }
        }
        eprintln!("[mir-callgraph] {crate_name}: {} edges, {} chunks ({fn_count} fns)", edges.len(), chunks.len());
    } else if json {
        use std::io::Write;
        let mut w = std::io::BufWriter::new(std::io::stdout().lock());
        for e in &edges { if let Ok(s) = serde_json::to_string(e) { let _ = writeln!(w, "{s}"); } }
    } else {
        eprintln!("[mir-callgraph] {crate_name}: {} edges, {} chunks", edges.len(), chunks.len());
    }

    ControlFlow::Break(())
}

// ── Main ────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();

    // Mode 1: RUSTC_WRAPPER mode
    let is_wrapper = args.get(1).is_some_and(|a| a.contains("rustc") && !a.starts_with("-"));

    if is_wrapper {
        let rustc_args: Vec<String> = args[2..].to_vec();

        let is_local = rustc_args.iter().any(|a| {
            a.ends_with(".rs") && !a.contains(".cargo") && !a.contains("registry") && !a.contains("rustup")
        });
        let has_edition = rustc_args.iter().any(|a| a.starts_with("--edition"));
        let is_build_script = rustc_args.iter().any(|a| a == "build_script_build" || a.contains("build.rs"));

        if has_edition && is_local && !is_build_script {
            let mut full_args = vec![args[1].clone()];
            full_args.extend(rustc_args.iter().cloned());

            if !full_args.iter().any(|a| a.starts_with("--sysroot")) {
                if let Ok(output) = Command::new(&args[1]).arg("--print").arg("sysroot").output() {
                    let sysroot = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !sysroot.is_empty() {
                        full_args.push("--sysroot".to_string());
                        full_args.push(sysroot);
                    }
                }
            }

            // Cache rustc args for direct mode
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
                    let env_snapshot: Vec<(String, String)> = env::vars()
                        .filter(|(k, _)| {
                            !matches!(k.as_str(), "PATH" | "PSModulePath" | "PATHEXT" | "CARGO_MAKEFLAGS")
                            && !k.starts_with("MIR_CALLGRAPH_")
                        })
                        .collect();
                    let cached = RustcArgs {
                        args: full_args.clone(), crate_name: crate_name.clone(), sysroot, env: env_snapshot,
                    };
                    let is_test = rustc_args.iter().any(|a| a == "--test");
                    let suffix = if is_test { ".test" } else { ".lib" };
                    if let Ok(json_str) = serde_json::to_string_pretty(&cached) {
                        let _ = std::fs::write(format!("{args_dir}/{crate_name}{suffix}.rustc-args.json"), json_str);
                    }
                }
            }

            let json = env::var("MIR_CALLGRAPH_JSON").is_ok();
            let is_test_target = rustc_args.iter().any(|a| a == "--test");
            let db_path = env::var("MIR_CALLGRAPH_DB").ok();
            let _ = rustc_public::run!(&full_args, || extract_all(is_test_target, json, &db_path));
        } else {
            let status = Command::new(&args[1]).args(&args[2..]).status().expect("failed to run rustc");
            std::process::exit(status.code().unwrap_or(1));
        }
        return;
    }

    // Mode 2: Direct mode
    if args.iter().any(|a| a == "--direct") {
        let args_files: Vec<&String> = args.iter()
            .skip_while(|a| *a != "--args-file").skip(1)
            .take_while(|a| !a.starts_with("--")).collect();

        if args_files.is_empty() {
            eprintln!("[mir-callgraph] --direct requires --args-file <path>");
            std::process::exit(1);
        }

        let json = env::var("MIR_CALLGRAPH_JSON").is_ok();
        let mut had_error = false;

        for args_file in &args_files {
            let content = match std::fs::read_to_string(args_file) {
                Ok(c) => c,
                Err(e) => { eprintln!("[mir-callgraph] read error {args_file}: {e}"); had_error = true; continue; }
            };
            let cached: RustcArgs = match serde_json::from_str(&content) {
                Ok(c) => c,
                Err(e) => { eprintln!("[mir-callgraph] parse error {args_file}: {e}"); had_error = true; continue; }
            };

            eprintln!("[mir-callgraph] direct: compiling crate '{}'", cached.crate_name);
            for (key, value) in &cached.env { unsafe { env::set_var(key, value); } }

            let is_test_target = cached.args.iter().any(|a| a == "--test");
            let db_path = env::var("MIR_CALLGRAPH_DB").ok();

            let _guard = if env::var("MIR_PROFILE").is_ok() {
                use tracing_subscriber::prelude::*;
                let (chrome_layer, guard) = tracing_chrome::ChromeLayerBuilder::new()
                    .file(format!("D:/rude/profile/{}.trace.json", cached.crate_name))
                    .include_args(true).build();
                tracing_subscriber::registry().with(chrome_layer).init();
                Some(guard)
            } else { None };

            let _ = rustc_public::run!(&cached.args, || extract_all(is_test_target, json, &db_path));
        }

        if had_error { std::process::exit(1); }
        return;
    }

    // Mode 3: CLI mode (cargo wrapper)
    let json = args.iter().any(|a| a == "--json");
    let exe = env::current_exe().unwrap_or_default();
    let keep_going = args.iter().any(|a| a == "--keep-going");

    let mut cmd = Command::new("cargo");
    cmd.arg("+nightly").arg("check").arg("--tests")
        .arg("--target-dir").arg("target/mir-check")
        .env("RUSTC_WRAPPER", &exe);
    if keep_going { cmd.arg("--keep-going"); }
    if json { cmd.env("MIR_CALLGRAPH_JSON", "1"); }
    for arg in args.iter().skip(1).filter(|a| *a != "--json" && *a != "--keep-going") { cmd.arg(arg); }

    let status = cmd.status().expect("failed to run cargo check");
    std::process::exit(status.code().unwrap_or(1));
}
