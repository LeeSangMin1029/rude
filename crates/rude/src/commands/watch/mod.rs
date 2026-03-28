mod handler;
mod watcher;

use std::path::Path;

pub use watcher::run;

const IGNORED_DIRS: &[&str] = &[".git", "target", "node_modules", "__pycache__"];

fn is_rust_source(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "rs")
}

fn is_in_ignored_dir(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| IGNORED_DIRS.contains(&s))
    })
}
