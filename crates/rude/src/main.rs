//! rude — Code intelligence CLI.

mod cli;
use rude_cli::commands;
use commands::edit::{apply_edits, Op};

use anyhow::Context as _;
use clap::Parser;

fn main() {
    #[allow(clippy::expect_used)]
    let directive = "rude=info".parse().expect("static directive");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive(directive),
        )
        .init();

    rude_db::interrupt::install_handler();

    // Run on a thread with 32 MB stack — graph build + SCIP parsing need deep stacks.
    let result = std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(run)
        .expect("failed to spawn main thread")
        .join()
        .expect("main thread panicked");

    if let Err(err) = result {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

/// RUDE_COMPACT=1 makes --compact the default for all commands.
fn env_compact() -> bool {
    std::env::var("RUDE_COMPACT").is_ok_and(|v| v == "1" || v == "true")
}

fn run() -> anyhow::Result<()> {
    use cli::{Cli, Commands};
    use commands::intel;

    let cli = Cli::parse();

    match cli.command {
        Commands::Aliases { db } => intel::run_aliases(db),
        Commands::Symbols { db, name, kind, include_tests, limit, compact } => {
            intel::run_symbols(db, name, kind, include_tests, limit, compact || env_compact())
        }
        Commands::Context { db, symbol, depth, source, include_tests, scope, tree, blast } => {
            intel::run_context(db, symbol, depth, source, include_tests, scope, tree, blast)
        }
        Commands::Trace { db, from, to } => {
            intel::run_trace(db, from, to)
        }
        Commands::Dupes { db, threshold, exclude_tests, k, json, ast, all, min_lines, min_sub_lines, analyze } => {
            commands::dupes::run(commands::dupes::DupesConfig {
                db, threshold, exclude_tests, k, json, ast_mode: ast, all_mode: all, min_lines, min_sub_lines, analyze,
            })
        }
        Commands::Dead { db, include_pub, file } => intel::run_dead(db, include_pub, file),
        Commands::Stats { db } => intel::run_stats(db),
        Commands::Coverage { db, file, refresh, .. } => intel::run_coverage(db, file, refresh),
        Commands::Add { db, input, exclude } => commands::add::run(db, input, &exclude),
        Commands::Replace { db, symbol, file, body, body_file } => {
            let body = read_body(body, body_file)?;
            apply_edits(&db, &[(&symbol, Op::Replace(&body))], file.as_deref())
        }
        Commands::InsertAfter { db, symbol, file, body, body_file } => {
            let body = read_body(body, body_file)?;
            apply_edits(&db, &[(&symbol, Op::After(&body))], file.as_deref())
        }
        Commands::InsertBefore { db, symbol, file, body, body_file } => {
            let body = read_body(body, body_file)?;
            apply_edits(&db, &[(&symbol, Op::Before(&body))], file.as_deref())
        }
        Commands::DeleteSymbol { db, symbol, file } => {
            apply_edits(&db, &[(&symbol, Op::Delete)], file.as_deref())
        }
        Commands::InsertAt { db, file, line, body, body_file } => {
            let body = read_body(body, body_file)?;
            commands::edit::insert_at(db, file, line, body)
        }
        Commands::DeleteLines { db, file, start, end } => {
            commands::edit::delete_lines(db, file, start, end)
        }
        Commands::ReplaceLines { db, file, start, end, body, body_file } => {
            let body = read_body(body, body_file)?;
            commands::edit::replace_lines(db, file, start, end, body)
        }
        Commands::Cluster { db, file, min_lines } => commands::cluster::run(db, file, min_lines),
        Commands::Watch { db, input } => commands::watch::run(db, input),
        Commands::CreateFile { db, file, body, body_file } => {
            let body = read_body(body, body_file)?;
            commands::edit::create_file(db, file, body)
        }
        Commands::Split { db, symbols, to, dry_run } => {
            commands::split::run(db, symbols, to, dry_run)
        }
    }
}

/// Read body from `--body`, `--body-file`, or stdin (in that priority).
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
