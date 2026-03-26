mod cli;
use rude_cli::commands;
use commands::edit::{apply_edits, Op};

use anyhow::Context as _;
use clap::Parser;

fn main() {
    let _guard = init_tracing();



    rude_db::interrupt::install_handler();

    // 32 MB stack: graph build + deep recursion in SCIP parsing can exhaust default stack.
    let result = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(run)
        .expect("failed to spawn main thread")
        .join()
        .expect("main thread panicked");

    drop(_guard);
    match result {
        Err(err) => { eprintln!("Error: {err}"); std::process::exit(1); }
        Ok(()) => {}
    }
}

fn env_compact() -> bool {
    std::env::var("RUDE_COMPACT").is_ok_and(|v| v == "1" || v == "true")
}

fn run() -> anyhow::Result<()> {
    use cli::{Cli, Commands};
    use commands::intel;

    let cli = Cli::parse();
    rude_cli::set_db(resolve_db(cli.db)?);

    match cli.command {
        Commands::Aliases => intel::run_aliases(),
        Commands::Symbols { name, kind, include_tests, limit, compact } => {
            intel::run_symbols(name, kind, include_tests, limit, compact || env_compact())
        }
        Commands::Context { symbol, depth, source, include_tests, scope, tree, blast } => {
            intel::run_context(symbol, depth, source, include_tests, scope, tree, blast)
        }
        Commands::Trace { from, to } => intel::run_trace(from, to),
        Commands::Dupes { threshold, exclude_tests, k, json, ast, all, min_lines, min_sub_lines, analyze } => {
            commands::dupes::run(commands::dupes::DupesConfig {
                threshold, exclude_tests, k, json, ast_mode: ast, all_mode: all, min_lines, min_sub_lines, analyze,
            })
        }
        Commands::Dead { include_pub, file } => intel::run_dead(include_pub, file),
        Commands::Stats => intel::run_stats(),
        Commands::Coverage { file, refresh, .. } => intel::run_coverage(file, refresh),
        Commands::Add { input, exclude } => commands::add::run(input, &exclude),
        Commands::Replace { symbol, file, body, body_file } => {
            let body = read_body(body, body_file)?;
            apply_edits(&[(&symbol, Op::Replace(&body))], file.as_deref())
        }
        Commands::InsertAfter { symbol, file, body, body_file } => {
            let body = read_body(body, body_file)?;
            apply_edits(&[(&symbol, Op::After(&body))], file.as_deref())
        }
        Commands::InsertBefore { symbol, file, body, body_file } => {
            let body = read_body(body, body_file)?;
            apply_edits(&[(&symbol, Op::Before(&body))], file.as_deref())
        }
        Commands::DeleteSymbol { symbol, file } => {
            apply_edits(&[(&symbol, Op::Delete)], file.as_deref())
        }
        Commands::InsertAt { file, line, body, body_file } => {
            let body = read_body(body, body_file)?;
            commands::edit::insert_at(file, line, body)
        }
        Commands::DeleteLines { file, start, end } => {
            commands::edit::delete_lines(file, start, end)
        }
        Commands::ReplaceLines { file, start, end, body, body_file } => {
            let body = read_body(body, body_file)?;
            commands::edit::replace_lines(file, start, end, body)
        }
        Commands::Batch { manifest } => {
            commands::edit::run_batch(manifest)
        }
        Commands::Cluster { file, min_lines } => commands::intel::run_cluster(file, min_lines),
        Commands::Watch { input } => commands::watch::run(input),
        Commands::CreateFile { file, body, body_file } => {
            let body = read_body(body, body_file)?;
            commands::edit::create_file(file, body)
        }
        Commands::Split { symbols, to, dry_run } => {
            commands::edit::split(symbols, to, dry_run)
        }
    }
}

fn init_tracing() -> Option<tracing_chrome::FlushGuard> {
    if std::env::var("RUDE_PROFILE").is_ok() {
        use tracing_subscriber::prelude::*;
        let (layer, guard) = tracing_chrome::ChromeLayerBuilder::new()
            .file("rude-profile.json").include_args(true).build();
        tracing_subscriber::registry().with(layer).init();
        Some(guard)
    } else {
        #[allow(clippy::expect_used)]
        let directive = "rude=info".parse().expect("static directive");
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive(directive))
            .init();
        None
    }
}

const DB_NAME: &str = ".code.db";

fn resolve_db(explicit: Option<std::path::PathBuf>) -> anyhow::Result<std::path::PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let mut dir = std::env::current_dir()?;
    loop {
        let candidate = dir.join(DB_NAME);
        if candidate.exists() {
            return Ok(candidate);
        }
        if !dir.pop() { break; }
    }
    anyhow::bail!("No {DB_NAME} found (searched from cwd upward). Pass db path explicitly.")
}

fn read_body(body: Option<String>, body_file: Option<std::path::PathBuf>) -> anyhow::Result<String> {
    if let Some(b) = body {
        return Ok(b);
    }
    if let Some(path) = body_file {
        return std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read body file: {}", path.display()));
    }
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}
