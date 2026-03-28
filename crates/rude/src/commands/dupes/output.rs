use serde::Serialize;
use rude_intel::clones::SubBlockClone;
use rude_intel::parse::{normalize_path, ParsedChunk};
use rude_util::{apply_alias, build_path_aliases};

use super::types::{
    ChunkLabel, DupePairLike, group_by_file, label, parse_label, print_json,
};

pub(crate) fn print_pairs_text(
    pairs: &[impl DupePairLike],
    chunks: &[ParsedChunk],
) {
    if pairs.is_empty() {
        println!("No duplicates found above threshold.");
        return;
    }
    println!("{} duplicate pairs found:\n", pairs.len());

    let idx_pairs: Vec<(usize, usize)> = pairs.iter().map(|p| (p.idx_a(), p.idx_b())).collect();
    let groups = group_by_file(&idx_pairs, chunks);

    for (file, entries) in &groups {
        println!("  {file} ({} pairs)", entries.len());
        for e in entries {
            let p = &pairs[e.pair_index];
            let score = p.display_score();
            let tag = p.display_tag();
            let tag_part = if tag.is_empty() { " ".to_owned() } else { format!(" {tag:<12} ") };
            println!("    [{score:.2}]{tag_part}{} \u{2194} {}", e.name_a, e.name_b);
        }
        println!();
    }

    eprintln!("Tip: rude context <symbol>  to inspect.");
}

pub(crate) fn print_pairs_json(pairs: &[impl DupePairLike], chunks: &[ParsedChunk]) {
    let vals: Vec<_> = pairs.iter()
        .map(|p| p.to_json_value(label(chunks, p.idx_a()), label(chunks, p.idx_b())))
        .collect();
    print_json(&serde_json::json!({ "pairs": vals }));
}

pub(crate) fn print_groups_text(groups: &[(u64, Vec<usize>)], chunks: &[ParsedChunk]) {
    if groups.is_empty() {
        println!("No clones found.");
        return;
    }
    println!("{} clone groups found:\n", groups.len());

    let entries_flat: Vec<(usize, u64, ChunkLabel)> = groups
        .iter()
        .enumerate()
        .flat_map(|(gi, (hash, indices))| {
            indices.iter().map(move |&idx| (gi + 1, *hash, parse_label(chunks, idx)))
        })
        .collect();

    let all_files: Vec<String> = entries_flat.iter().map(|(_, _, cl)| normalize_path(&cl.file)).collect();
    let (alias_map, _) = build_path_aliases(&all_files.iter().map(String::as_str).collect::<Vec<_>>());

    use std::collections::HashMap;
    type FileGroup = (String, Vec<(usize, u64, String)>);
    let mut by_file: Vec<FileGroup> = Vec::new();
    let mut file_index: HashMap<String, usize> = HashMap::new();

    for (i, (group_num, hash, cl)) in entries_flat.iter().enumerate() {
        let key = if all_files[i].is_empty() { "(unknown)".to_owned() } else { apply_alias(&all_files[i], &alias_map) };
        let idx = *file_index.entry(key.clone()).or_insert_with(|| {
            let n = by_file.len();
            by_file.push((key, Vec::new()));
            n
        });
        by_file[idx].1.push((*group_num, *hash, cl.name.clone()));
    }

    by_file.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    for (file, entries) in &by_file {
        println!("  {file} ({} clones)", entries.len());
        for (group_num, hash, name) in entries {
            println!("    G{group_num} [{hash:016x}]  {name}");
        }
        println!();
    }

    eprintln!("Tip: rude context <symbol>  to inspect.");
}

pub(crate) fn print_groups_json(groups: &[(u64, Vec<usize>)], chunks: &[ParsedChunk]) {
    #[derive(Serialize)]
    struct GroupJson { hash: String, members: Vec<String> }
    #[derive(Serialize)]
    struct Out { groups: Vec<GroupJson> }
    print_json(&Out {
        groups: groups.iter().map(|(hash, indices)| GroupJson {
            hash: format!("{hash:016x}"),
            members: indices.iter().map(|&idx| label(chunks, idx)).collect(),
        }).collect(),
    });
}

pub(crate) fn print_sub_block_text(clones: &[SubBlockClone], chunks: &[ParsedChunk]) {
    println!("\n{} sub-block clones (intra-function):\n", clones.len());

    let labels: Vec<(ChunkLabel, ChunkLabel)> = clones
        .iter()
        .map(|c| (parse_label(chunks, c.chunk_idx_a), parse_label(chunks, c.chunk_idx_b)))
        .collect();
    let all_files: Vec<String> = labels
        .iter()
        .flat_map(|(a, b)| [normalize_path(&a.file), normalize_path(&b.file)])
        .collect();
    let (alias_map, _) = build_path_aliases(&all_files.iter().map(String::as_str).collect::<Vec<_>>());

    for (i, c) in clones.iter().enumerate() {
        let (name_a, name_b) = (&labels[i].0.name, &labels[i].1.name);
        let file_a = apply_alias(&all_files[i * 2], &alias_map);
        let file_b = apply_alias(&all_files[i * 2 + 1], &alias_map);
        let body_tag = if c.body_match { " [exact]" } else { "" };
        println!(
            "    {name_a}  ({file_a} L{}-{}) \u{2194} {name_b}  ({file_b} L{}-{}){body_tag}",
            c.block_a_start + 1, c.block_a_end + 1, c.block_b_start + 1, c.block_b_end + 1,
        );
    }
    println!();
}

pub(crate) fn print_sub_block_json(clones: &[SubBlockClone], chunks: &[ParsedChunk]) {
    #[derive(Serialize)]
    struct SubBlockJson { a: String, a_lines: [usize; 2], b: String, b_lines: [usize; 2], body_match: bool }
    #[derive(Serialize)]
    struct Out { sub_block_clones: Vec<SubBlockJson> }
    print_json(&Out {
        sub_block_clones: clones.iter().map(|c| SubBlockJson {
            a: label(chunks, c.chunk_idx_a),
            a_lines: [c.block_a_start + 1, c.block_a_end + 1],
            b: label(chunks, c.chunk_idx_b),
            b_lines: [c.block_b_start + 1, c.block_b_end + 1],
            body_match: c.body_match,
        }).collect(),
    });
}
