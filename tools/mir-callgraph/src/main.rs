#![feature(rustc_private)]
extern crate rustc_driver;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_public;

mod cli;
mod daemon;
mod direct;
mod extract;
mod output;
mod types;
mod wrapper;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match detect_mode(&args) {
        Mode::Wrapper => wrapper::run(&args),
        Mode::Daemon  => daemon::run(&args),
        Mode::Direct  => direct::run(&args),
        Mode::Cli     => cli::run(&args),
    }
}

enum Mode { Wrapper, Daemon, Direct, Cli }

fn detect_mode(args: &[String]) -> Mode {
    if args.get(1).is_some_and(|a| a.contains("rustc") && !a.starts_with("-")) {
        Mode::Wrapper
    } else if args.iter().any(|a| a == "--daemon") {
        Mode::Daemon
    } else if args.iter().any(|a| a == "--direct") {
        Mode::Direct
    } else {
        Mode::Cli
    }
}

pub fn env_config() -> (bool, Option<String>) {
    (std::env::var("MIR_CALLGRAPH_JSON").is_ok(), std::env::var("MIR_CALLGRAPH_DB").ok())
}

pub fn load_cached_args(path: &str) -> Result<types::RustcArgs, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read error {path}: {e}"))?;
    serde_json::from_str(&content).map_err(|e| format!("parse error {path}: {e}"))
}
