use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use rude_intel::clones::{DupePair, UnifiedDupePair};
use rude_intel::parse::{normalize_path, ParsedChunk};
use rude_util::apply_alias;

pub(crate) trait DupePairLike {
    fn idx_a(&self) -> usize;
    fn idx_b(&self) -> usize;
    fn display_score(&self) -> f32;
    fn display_tag(&self) -> String { String::new() }
    fn to_json_value(&self, a: String, b: String) -> serde_json::Value;
}

impl DupePairLike for DupePair {
    fn idx_a(&self) -> usize { self.idx_a }
    fn idx_b(&self) -> usize { self.idx_b }
    fn display_score(&self) -> f32 { self.similarity }
    fn to_json_value(&self, a: String, b: String) -> serde_json::Value {
        serde_json::json!({ "a": a, "b": b, "sim": self.similarity })
    }
}

impl DupePairLike for UnifiedDupePair {
    fn idx_a(&self) -> usize { self.idx_a }
    fn idx_b(&self) -> usize { self.idx_b }
    fn display_score(&self) -> f32 { self.score }
    fn display_tag(&self) -> String { self.tag() }
    fn to_json_value(&self, a: String, b: String) -> serde_json::Value {
        serde_json::json!({ "a": a, "b": b, "score": self.score, "jaccard": self.jaccard, "ast_match": self.ast_match, "tag": self.tag() })
    }
}

pub(crate) struct GroupEntry {
    pub pair_index: usize,
    pub name_a: String,
    pub name_b: String,
}

pub(crate) struct ChunkLabel {
    pub name: String,
    pub file: String,
}

impl ChunkLabel {
    pub fn display(&self) -> String {
        if self.file.is_empty() {
            self.name.clone()
        } else {
            format!("{}  ({})", self.name, self.file)
        }
    }
}

pub(crate) fn parse_label(chunks: &[ParsedChunk], idx: usize) -> ChunkLabel {
    let c = &chunks[idx];
    let name = if c.name.is_empty() {
        format!("idx:{idx}")
    } else {
        c.name.clone()
    };
    ChunkLabel { name, file: c.file.clone() }
}

pub(crate) fn label(chunks: &[ParsedChunk], idx: usize) -> String {
    parse_label(chunks, idx).display()
}

#[expect(clippy::expect_used)]
pub(crate) fn print_json<T: Serialize>(val: &T) {
    println!("{}", serde_json::to_string(val).expect("JSON serialize"));
}

pub(crate) fn group_by_file(
    idx_pairs: &[(usize, usize)],
    chunks: &[ParsedChunk],
    alias_map: &BTreeMap<String, String>,
) -> Vec<(String, Vec<GroupEntry>)> {
    let labels: Vec<(ChunkLabel, ChunkLabel)> = idx_pairs
        .iter()
        .map(|&(a, b)| (parse_label(chunks, a), parse_label(chunks, b)))
        .collect();

    let all_files: Vec<String> = labels
        .iter()
        .flat_map(|(a, b)| [normalize_path(&a.file), normalize_path(&b.file)])
        .collect();

    let mut by_file: Vec<(String, Vec<GroupEntry>)> = Vec::new();
    let mut file_index: HashMap<String, usize> = HashMap::new();

    for (i, _) in idx_pairs.iter().enumerate() {
        let raw = &all_files[i * 2];
        let key = if raw.is_empty() { "(unknown)".to_owned() } else { apply_alias(raw, alias_map) };
        let idx = *file_index.entry(key.clone()).or_insert_with(|| {
            let i = by_file.len();
            by_file.push((key, Vec::new()));
            i
        });
        by_file[idx].1.push(GroupEntry {
            pair_index: i,
            name_a: labels[i].0.name.clone(),
            name_b: labels[i].1.name.clone(),
        });
    }

    by_file.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    by_file
}
