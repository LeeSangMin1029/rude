use std::env;
use crate::extract;
use crate::types::RustcArgs;

pub fn run(args: &[String]) {
    let args_files: Vec<&String> = args.iter()
        .skip_while(|a| *a != "--args-file").skip(1)
        .take_while(|a| !a.starts_with("--")).collect();
    if args_files.is_empty() {
        eprintln!("[mir-callgraph] --direct requires --args-file <path>");
        std::process::exit(1);
    }
    let (json, _) = crate::types::env_config();
    let mut had_error = false;
    for args_file in &args_files {
        let cached: RustcArgs = match crate::types::RustcArgs::load(args_file) {
            Ok(c) => c,
            Err(e) => { eprintln!("[mir-callgraph] {e}"); had_error = true; continue; }
        };
        eprintln!("[mir-callgraph] direct: compiling crate '{}'", cached.crate_name);
        for (k, v) in &cached.env { unsafe { env::set_var(k, v); } }
        let is_test = cached.args.iter().any(|a| a == "--test");
        let db_path = env::var("MIR_CALLGRAPH_DB").ok();
        if let Err(e) = rustc_public::run!(&cached.args, || extract::extract_all(is_test, json, &db_path)) {
            eprintln!("[mir-callgraph] run! error: {e:?}");
        }
    }
    if had_error { std::process::exit(1); }
}
