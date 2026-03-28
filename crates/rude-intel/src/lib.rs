pub mod data;
pub mod graph;
pub mod analysis;
pub mod mir_edges;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static DB_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn set_db(path: PathBuf) {
    DB_PATH.set(path).ok();
}

pub fn db() -> &'static Path {
    DB_PATH.get().expect("rude_intel::set_db() not called")
}

pub use data::minhash;
pub use data::parse;

pub use graph::bfs;
pub use graph::context;
pub use graph::context_cmd;
pub use graph::edge_resolve;
pub use graph::impact;
pub use graph::index_tables;
pub use graph::jump;
pub use graph::trace;

pub use analysis::clones;
pub use analysis::dupe_analyze;
pub use analysis::loader;
pub use analysis::stats;

#[cfg(test)]
mod tests;
