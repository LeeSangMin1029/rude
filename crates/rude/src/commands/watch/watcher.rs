use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecursiveMode, Watcher};

use super::{is_in_ignored_dir, is_rust_source};

pub fn run(input_path: PathBuf) -> Result<()> {
    let db_path = crate::db().to_path_buf();
    rude_intel::parse::set_project_root(&input_path);

    println!("[watch] Watching {} for changes...", input_path.display());
    println!("[watch] DB: {}", db_path.display());
    println!("[watch] Press Ctrl+C to stop\n");

    if !db_path.exists() {
        eprintln!("[watch] No DB found, running initial add...");
        crate::commands::add::run(input_path.clone(), &[])?;
        eprintln!("[watch] Initial build complete\n");
    }

    let (tx, rx) = mpsc::channel::<PathBuf>();

    let sender = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };

        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ) {
            return;
        }

        for path in event.paths {
            if is_rust_source(&path) && !is_in_ignored_dir(&path) {
                let _ = sender.send(path);
            }
        }
    })
    .context("failed to create file watcher")?;

    watcher
        .watch(&input_path, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", input_path.display()))?;

    let mut pending = HashSet::new();

    loop {
        match rx.recv() {
            Ok(path) => {
                pending.insert(path);
            }
            Err(_) => {
                eprintln!("[watch] Channel closed, stopping");
                break;
            }
        }

        while let Ok(path) = rx.try_recv() {
            pending.insert(path);
        }

        let changed: Vec<PathBuf> = pending.drain().collect();
        super::handler::process_changes(&changed, &db_path, &input_path);
    }

    Ok(())
}
