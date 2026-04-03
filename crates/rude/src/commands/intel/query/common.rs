use std::collections::BTreeMap;


use rude_intel::graph;
use rude_util::{apply_alias, format_lines_opt, relative_path, shorten_signature};

pub(crate) struct TaggedEntry {
    pub idx: u32,
    pub tag: &'static str,
    pub sig: bool,
    pub call_line: u32,
}

pub(crate) fn load_or_build_graph() -> anyhow::Result<rude_intel::graph::CallGraph> {
    rude_intel::loader::load_or_build_graph()
}

pub(crate) fn save_query_context(graph: &graph::CallGraph, seeds: &[u32]) {
    let entries: Vec<String> = seeds.iter()
        .filter_map(|&i| {
            let c = graph.chunks.get(i as usize)?;
            Some(c.dn().to_string())
        })
        .collect();
    if entries.is_empty() { return; }
    let db = crate::db();
    let Ok(engine) = rude_db::StorageEngine::open(db) else { return };
    let mut recent: Vec<String> = engine.get_cache("recent_query_names").ok()
        .flatten()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    for f in &entries {
        recent.retain(|r| r != f);
        recent.insert(0, f.clone());
    }
    recent.truncate(30);
    if let Ok(json) = serde_json::to_vec(&recent) {
        let _ = engine.set_cache("recent_query_names", &json);
    }
}

pub(crate) fn rank_by_recent(graph: &graph::CallGraph, seeds: &mut Vec<u32>) {
    if seeds.len() <= 1 { return; }
    let db = crate::db();
    let Ok(engine) = rude_db::StorageEngine::open(db) else { return };
    let recent: Vec<String> = engine.get_cache("recent_query_names").ok()
        .flatten()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if recent.is_empty() { return; }
    seeds.sort_by(|&a, &b| {
        let sa = score_chunk(&graph.chunks[a as usize], &recent);
        let sb = score_chunk(&graph.chunks[b as usize], &recent);
        sb.cmp(&sa)
    });
}

fn score_chunk(chunk: &rude_intel::parse::ParsedChunk, recent: &[String]) -> u32 {
    let dn = chunk.dn();
    let mut best = 0u32;
    let dn_base = dn.split('<').next().unwrap_or(dn);
    for (i, r) in recent.iter().enumerate() {
        let recency = (30 - i as u32).max(1);
        let r_base = r.split('<').next().unwrap_or(r);
        if dn_base == r_base {
            return 100 + recency;
        }
        if let Some(owner) = dn_base.rsplit_once("::").map(|(o, _)| o) {
            if owner == r_base || r_base.ends_with(owner) || owner.ends_with(r_base) {
                best = best.max(80 + recency);
            }
        }
    }
    best
}

pub(super) fn resolve_symbol(graph: &graph::CallGraph, symbol: &str) -> Option<Vec<u32>> {
    let seeds = graph.resolve(symbol);
    if seeds.is_empty() { println!("No symbol found matching \"{symbol}\"."); None } else { Some(seeds) }
}

#[inline]
pub(super) fn fmt_scope(scope: &Option<String>) -> String {
    scope.as_ref().map_or(String::new(), |s| format!(" (scope: {s})"))
}

pub(super) fn print_tagged(
    graph: &graph::CallGraph,
    entries: &[TaggedEntry],
    show_source: bool,
    alias_map: &BTreeMap<String, String>,
) {
    if entries.is_empty() { return; }

    let role_priority = |tag: &str| -> u8 {
        match tag { "def" | "target" | "field" => 0, "caller" | "d1" | "d2" | "d3+" => 1, "callee" => 2, "type" => 3, "test" => 4, _ => 5 }
    };
    let mut seen = std::collections::HashSet::<u32>::new();
    let mut sorted: Vec<&TaggedEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| role_priority(e.tag));
    let deduped: Vec<&TaggedEntry> = sorted.into_iter().filter(|e| seen.insert(e.idx)).collect();

    let files: Vec<&str> = deduped.iter().map(|e| relative_path(&graph.chunks[e.idx as usize].file)).collect();
    let mut groups: BTreeMap<&str, Vec<&TaggedEntry>> = BTreeMap::new();
    for (entry, file) in deduped.iter().zip(files.iter()) { groups.entry(file).or_default().push(entry); }

    for (file, items) in &groups {
        println!("@ {}", apply_alias(file, alias_map));
        for e in items {
            let i = e.idx as usize;
            let kind_tag = if graph.chunks[i].kind == "function" { String::new() } else { format!("[{}] ", graph.chunks[i].kind) };
            let test_marker = if graph.is_test[i] { " [test]" } else { "" };
            let call_site = if e.call_line > 0 { format!(" → :{}", e.call_line) } else { String::new() };
            println!("  [{}] {} {kind_tag}{}{test_marker}{call_site}", e.tag, format_lines_opt(graph.chunks[i].lines), graph.chunks[i].dn());
            if e.sig {
                if let Some(s) = &graph.chunks[i].signature {
                    if !s.is_empty() { println!("    {}", shorten_signature(s, 120)); }
                }
            }
            if show_source && (e.tag == "def" || e.tag == "test" || e.sig) {
                if let Some((start, end)) = graph.chunks[i].lines {
                    let file_path = &graph.chunks[i].file;
                    let abs_file = if std::path::Path::new(file_path).exists() {
                        std::path::PathBuf::from(file_path)
                    } else {
                        rude_util::find_project_root(crate::db())
                            .map(|r| r.join(file_path))
                            .unwrap_or_else(|| std::path::PathBuf::from(file_path))
                    };
                    if let Ok(content) = std::fs::read_to_string(&abs_file) {
                        let lines: Vec<&str> = content.lines().collect();
                        let s = start.saturating_sub(1);
                        let e2 = end.min(lines.len());
                        if s < e2 {
                            println!("    ```");
                            for (j, ln) in lines[s..e2].iter().enumerate() { println!("    {:>4}│ {ln}", s + j + 1); }
                            println!("    ```");
                        }
                    }
                }
            }
        }
        println!();
    }
}
