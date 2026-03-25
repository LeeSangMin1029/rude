use crate::graph::bfs::HasIdx;

pub struct BfsEntry {
    pub idx: u32,
    pub depth: u32,
    pub score: f64,
}

impl HasIdx for BfsEntry {
    fn idx(&self) -> u32 { self.idx }
}
