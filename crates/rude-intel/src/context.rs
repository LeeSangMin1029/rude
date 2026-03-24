//! BFS forward traversal (context) — what a symbol calls.
//!
//! Shows the "context" of a symbol: what it calls (callees), what those
//! call, etc., up to a configurable depth.

use crate::bfs::HasIdx;

/// BFS result entry with depth and score.
pub struct BfsEntry {
    pub idx: u32,
    pub depth: u32,
    pub score: f64,
}

impl HasIdx for BfsEntry {
    fn idx(&self) -> u32 { self.idx }
}

