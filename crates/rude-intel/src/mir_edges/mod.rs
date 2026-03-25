//! MIR-based call edge extraction via `mir-callgraph` subprocess.
//!
//! Parses JSONL output from the `mir-callgraph` tool and provides
//! resolved call edges for accurate graph construction.

pub mod types;
pub mod sqlite;
pub mod runner;
pub mod workspace;

// Re-export public types
pub use types::{CalleeInfo, MirChunk, MirEdgeMap, mir_chunks_to_parsed, parse_calls_field};

// Re-export public sqlite functions
pub use sqlite::{clear_mir_db, mir_db_path};

// Re-export public runner functions
pub use runner::{run_mir_callgraph, run_mir_callgraph_for, run_mir_direct};

// Re-export public workspace functions
pub use workspace::{detect_changed_crates, detect_missing_edge_crates};
