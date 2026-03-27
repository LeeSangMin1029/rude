#![feature(rustc_private)]
extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_public;
extern crate rustc_span;

mod cli;
mod daemon;
mod direct;
mod extract;
mod output;
pub mod types;
mod wrapper;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match detect_mode(&args) {
        Mode::Wrapper => wrapper::run(&args),
        Mode::Worker  => daemon::worker_run(),
        Mode::Daemon  => daemon::run(&args),
        Mode::Direct  => direct::run(&args),
        Mode::Cli     => cli::run(&args),
    }
}

enum Mode { Wrapper, Daemon, Direct, Cli, Worker }

fn detect_mode(args: &[String]) -> Mode {
    if args.get(1).is_some_and(|a| a.contains("rustc") && !a.starts_with("-")) {
        Mode::Wrapper
    } else if args.iter().any(|a| a == "--worker") {
        Mode::Worker
    } else if args.iter().any(|a| a == "--daemon") {
        Mode::Daemon
    } else if args.iter().any(|a| a == "--direct") {
        Mode::Direct
    } else {
        Mode::Cli
    }
}
