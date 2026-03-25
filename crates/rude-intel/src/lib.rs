//! Code intelligence library — structural queries on code-chunked databases.
//!
//! Provides reusable types and algorithms for code navigation:
//! parsing, call graph construction, BFS traversal, and statistics.
//!
//! CLI command handlers live in `rude-cli`; this crate contains only
//! the pure analysis logic.

pub mod data;
pub mod graph;
pub mod analysis;
pub mod mir_edges;

// Re-export flat module paths for backward compatibility.
// External crates use `rude_intel::parse::ParsedChunk` etc.
pub use data::chunk_types;
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
pub use analysis::helpers;
pub use analysis::loader;
pub use analysis::stats;

#[cfg(test)]
mod tests;
