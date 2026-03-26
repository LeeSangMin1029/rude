use std::env;
use std::process::Command;
use crate::types::RustcArgs;
use crate::extract;

pub fn run(args: &[String]) {
    let rustc_args: Vec<String> = args[2..].to_vec();
    if !should_analyze(&rustc_args) {
        let status = Command::new(&args[1]).args(&args[2..]).status().expect("failed to run rustc");
        std::process::exit(status.code().unwrap_or(1));
    }
    let full_args = build_full_args(&args[1], &rustc_args);
    cache_rustc_args(&rustc_args, &full_args);
    let (json, db_path) = crate::env_config();
    let is_test = rustc_args.iter().any(|a| a == "--test");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rustc_public::run!(&full_args, || extract::extract_all(is_test, json, &db_path))
    }));
    if let Err(panic) = result { eprintln!("[mir-callgraph] panic: {panic:?}"); }
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
            if !sysroot.is_empty() { full.extend(["--sysroot".to_string(), sysroot]); }
        }
    }
    full
}

fn cache_rustc_args(rustc_args: &[String], full_args: &[String]) {
    let Ok(out_dir) = env::var("MIR_CALLGRAPH_OUT") else { return };
    let crate_name = rustc_args.iter()
        .position(|a| a == "--crate-name").and_then(|i| rustc_args.get(i + 1))
        .cloned().unwrap_or_default();
    if crate_name.is_empty() { return; }
    let sysroot = full_args.iter()
        .position(|a| a == "--sysroot").and_then(|i| full_args.get(i + 1))
        .cloned().unwrap_or_default();
    let args_dir = format!("{out_dir}/rustc-args");
    let _ = std::fs::create_dir_all(&args_dir);
    let env_snapshot: Vec<(String, String)> = env::vars()
        .filter(|(k, _)| {
            !matches!(k.as_str(), "PATH" | "PSModulePath" | "PATHEXT" | "CARGO_MAKEFLAGS")
            && !k.starts_with("MIR_CALLGRAPH_")
        }).collect();
    let cached = RustcArgs { args: full_args.to_vec(), crate_name: crate_name.clone(), sysroot, env: env_snapshot };
    let suffix = if rustc_args.iter().any(|a| a == "--test") { ".test" } else { ".lib" };
    if let Ok(json) = serde_json::to_string_pretty(&cached) {
        let _ = std::fs::write(format!("{args_dir}/{crate_name}{suffix}.rustc-args.json"), json);
    }
}
