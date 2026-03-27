
use std::collections::BTreeMap;

use crate::graph::index_tables::is_test_chunk;
use rude_util::extract_crate_name;
use crate::data::parse::ParsedChunk;

pub fn build_stats(chunks: &[ParsedChunk]) -> BTreeMap<String, [usize; 4]> {
    let mut stats: BTreeMap<String, [usize; 4]> = BTreeMap::new();
    for c in chunks {
        let crate_name = extract_crate_name(&c.file);
        let row = stats.entry(crate_name).or_insert([0; 4]);
        let is_test = is_test_chunk(c);
        match (c.kind.as_str(), is_test) {
            ("function", false) => row[0] += 1,
            ("function", true) => row[1] += 1,
            ("struct", _) => row[2] += 1,
            ("enum", _) => row[3] += 1,
            _ => {}
        }
    }
    stats
}
