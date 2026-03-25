// rustc_private + extern crates required by rustc_public::run!() macro.
// Will be removable when rustc_public is published to crates.io.
#![feature(rustc_private)]
extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_public;

mod extract;
mod output;
mod types;

use std::env;
use std::process::Command;

use types::RustcArgs;

fn main() {
    let args: Vec<String> = env::args().collect();

    if is_rustc_wrapper(&args) {
        run_wrapper_mode(&args);
    } else if args.iter().any(|a| a == "--direct") {
        run_direct_mode(&args);
    } else {
        run_cli_mode(&args);
    }
}

// ── Mode detection ──────────────────────────────────────────────────

fn is_rustc_wrapper(args: &[String]) -> bool {
    args.get(1).is_some_and(|a| a.contains("rustc") && !a.starts_with("-"))
}

fn env_config() -> (bool, Option<String>) {
    (env::var("MIR_CALLGRAPH_JSON").is_ok(), env::var("MIR_CALLGRAPH_DB").ok())
}

// ── Mode 1: RUSTC_WRAPPER ───────────────────────────────────────────

fn run_wrapper_mode(args: &[String]) {
    let rustc_args: Vec<String> = args[2..].to_vec();

    if !should_analyze(&rustc_args) {
        let status = Command::new(&args[1]).args(&args[2..]).status().expect("failed to run rustc");
        std::process::exit(status.code().unwrap_or(1));
    }

    let full_args = build_full_args(&args[1], &rustc_args);
    cache_rustc_args(&rustc_args, &full_args);

    let (json, db_path) = env_config();
    let is_test = rustc_args.iter().any(|a| a == "--test");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rustc_public::run!(&full_args, || extract::extract_all(is_test, json, &db_path))
    }));
    if let Err(panic) = result {
        eprintln!("[mir-callgraph] panic: {panic:?}");
    }
}

fn should_analyze(rustc_args: &[String]) -> bool {
    let is_local = rustc_args.iter().any(|a| {
        a.ends_with(".rs") && !a.contains(".cargo") && !a.contains("registry") && !a.contains("rustup")
    });
    let has_edition = rustc_args.iter().any(|a| a.starts_with("--edition"));
    let is_build_script = rustc_args.iter().any(|a| a == "build_script_build" || a.contains("build.rs"));
    has_edition && is_local && !is_build_script
}

fn build_full_args(rustc_bin: &str, rustc_args: &[String]) -> Vec<String> {
    let mut full = vec![rustc_bin.to_string()];
    full.extend(rustc_args.iter().cloned());

    if !full.iter().any(|a| a.starts_with("--sysroot")) {
        if let Ok(output) = Command::new(rustc_bin).arg("--print").arg("sysroot").output() {
            let sysroot = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !sysroot.is_empty() {
                full.extend(["--sysroot".to_string(), sysroot]);
            }
        }
    }
    full
}

fn cache_rustc_args(rustc_args: &[String], full_args: &[String]) {
    let Ok(out_dir) = env::var("MIR_CALLGRAPH_OUT") else { return };
    let crate_name = rustc_args.iter()
        .position(|a| a == "--crate-name")
        .and_then(|i| rustc_args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    if crate_name.is_empty() { return; }

    let sysroot = full_args.iter()
        .position(|a| a == "--sysroot")
        .and_then(|i| full_args.get(i + 1))
        .cloned()
        .unwrap_or_default();

    let args_dir = format!("{out_dir}/rustc-args");
    let _ = std::fs::create_dir_all(&args_dir);

    let env_snapshot: Vec<(String, String)> = env::vars()
        .filter(|(k, _)| {
            !matches!(k.as_str(), "PATH" | "PSModulePath" | "PATHEXT" | "CARGO_MAKEFLAGS")
            && !k.starts_with("MIR_CALLGRAPH_")
        })
        .collect();

    let cached = RustcArgs {
        args: full_args.to_vec(), crate_name: crate_name.clone(), sysroot, env: env_snapshot,
    };
    let suffix = if rustc_args.iter().any(|a| a == "--test") { ".test" } else { ".lib" };
    if let Ok(json) = serde_json::to_string_pretty(&cached) {
        let _ = std::fs::write(format!("{args_dir}/{crate_name}{suffix}.rustc-args.json"), json);
    }
}

// ── Mode 2: Direct ──────────────────────────────────────────────────

fn run_direct_mode(args: &[String]) {
    let args_files: Vec<&String> = args.iter()
        .skip_while(|a| *a != "--args-file").skip(1)
        .take_while(|a| !a.starts_with("--")).collect();

    if args_files.is_empty() {
        eprintln!("[mir-callgraph] --direct requires --args-file <path>");
        std::process::exit(1);
    }

    let (json, _) = env_config();
    let mut had_error = false;

    for args_file in &args_files {
        let cached: RustcArgs = match load_cached_args(args_file) {
            Ok(c) => c,
            Err(e) => { eprintln!("[mir-callgraph] {e}"); had_error = true; continue; }
        };

        eprintln!("[mir-callgraph] direct: compiling crate '{}'", cached.crate_name);
        for (k, v) in &cached.env { unsafe { env::set_var(k, v); } }

        let is_test = cached.args.iter().any(|a| a == "--test");
        let db_path = env::var("MIR_CALLGRAPH_DB").ok();

        let _guard = init_profiling(&cached.crate_name);

        if let Err(e) = rustc_public::run!(&cached.args, || extract::extract_all(is_test, json, &db_path)) {
            eprintln!("[mir-callgraph] run! error: {e:?}");
        }
    }

    if had_error { std::process::exit(1); }
}

fn load_cached_args(path: &str) -> Result<RustcArgs, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read error {path}: {e}"))?;
    serde_json::from_str(&content).map_err(|e| format!("parse error {path}: {e}"))
}

fn init_profiling(crate_name: &str) -> Option<tracing_chrome::FlushGuard> {
    if env::var("MIR_PROFILE").is_err() { return None; }
    use tracing_subscriber::prelude::*;
    let (layer, guard) = tracing_chrome::ChromeLayerBuilder::new()
        .file(format!("profile/{crate_name}.trace.json"))
        .include_args(true).build();
    tracing_subscriber::registry().with(layer).init();
    Some(guard)
}

// ── Mode 3: CLI (cargo wrapper) ─────────────────────────────────────

fn run_cli_mode(args: &[String]) {
    let json = args.iter().any(|a| a == "--json");
    let keep_going = args.iter().any(|a| a == "--keep-going");
    let exe = env::current_exe().unwrap_or_default();
    let extra: Vec<&String> = args.iter().skip(1)
        .filter(|a| *a != "--json" && *a != "--keep-going").collect();
    let has_package_flag = extra.iter().any(|a| *a == "-p" || a.starts_with("--package"));

    // If no -p flag, resolve local workspace packages to avoid ambiguous spec errors.
    let packages: Vec<String> = if has_package_flag {
        Vec::new()
    } else {
        local_workspace_packages()
    };

    let mut cmd = Command::new("cargo");
    cmd.arg("+nightly").arg("check").arg("--tests")
        .arg("--target-dir").arg("target/mir-check")
        .env("RUSTC_WRAPPER", &exe);
    if keep_going { cmd.arg("--keep-going"); }
    if json { cmd.env("MIR_CALLGRAPH_JSON", "1"); }
    for arg in &extra { cmd.arg(arg); }
    if !has_package_flag {
        for pkg in &packages { cmd.arg("-p").arg(pkg); }
    }

    let status = cmd.status().expect("failed to run cargo check");
    std::process::exit(status.code().unwrap_or(1));
}

fn local_workspace_packages() -> Vec<String> {
    let output = Command::new("cargo").args(["metadata", "--no-deps", "--format-version", "1"])
        .output().ok();
    let Some(out) = output.filter(|o| o.status.success()) else { return Vec::new() };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Vec::new() };
    // Use package ID (e.g. "path+file:///...#name@ver") to avoid ambiguous specs
    json.get("packages").and_then(|p| p.as_array()).map(|pkgs| {
        pkgs.iter().filter_map(|p| {
            let id = p.get("id")?.as_str()?;
            // Only include local (path-based) packages
            if id.starts_with("path+") { Some(id.to_owned()) } else { None }
        }).collect()
    }).unwrap_or_default()
}
