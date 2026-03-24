//! Code intelligence library — structural queries on code-chunked databases.
//!
//! Provides reusable types and algorithms for code navigation:
//! parsing, call graph construction, BFS traversal, and statistics.
//!
//! CLI command handlers live in `rude-cli`; this crate contains only
//! the pure analysis logic.

pub mod bfs;
pub mod clones;
pub mod context;
pub mod context_cmd;
pub mod dupe_analyze;
pub mod chunk_types;
pub mod edge_resolve;
pub mod graph;
pub mod minhash;
pub mod helpers;
pub mod index_tables;
pub mod impact;
pub mod jump;
pub mod loader;
pub mod parse;
pub mod stats;
pub mod mir_edges;
pub mod trace;

#[cfg(test)]
mod tests;
