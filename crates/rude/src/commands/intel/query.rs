use std::collections::BTreeMap;

use anyhow::Result;

use rude_intel::graph;
use rude_util::{apply_alias, build_path_aliases, format_lines_opt, relative_path};
use rude_intel::impact;
use rude_intel::parse::ParsedChunk;
use rude_intel::stats::build_stats;
use rude_intel::trace;

pub(crate) fn load_or_build_graph() -> Result<graph::CallGraph> {
    rude_intel::loader::load_or_build_graph(crate::db())
}
pub fn run_aliases() -> Result<()> {
    let graph = load_or_build_graph()?;
    let (_alias_map, legend) = graph.global_aliases();
    for (alias, dir) in &legend { println!("{alias} = {dir}"); }
    Ok(())
}

pub fn run_stats() -> Result<()> {
    let graph = load_or_build_graph()?;
    let chunks = &graph.chunks;
    let stats = build_stats(&chunks);
    println!("=== stats: {} crates ===\n", stats.len());
    println!("{:<24} {:>8} {:>8} {:>8} {:>8}", "crate", "prod_fn", "test_fn", "struct", "enum");
    println!("{}", "-".repeat(60));
    let mut totals = [0usize; 4];
    for (name, row) in &stats {
        println!("{:<24} {:>8} {:>8} {:>8} {:>8}", name, row[0], row[1], row[2], row[3]);
        for (i, v) in row.iter().enumerate() { totals[i] += v; }
    }
    println!("{}", "-".repeat(60));
    println!("{:<24} {:>8} {:>8} {:>8} {:>8}", "total", totals[0], totals[1], totals[2], totals[3]);
    Ok(())
}

pub fn run_symbols(
    name: Option<String>,
    kind: Option<String>,
    include_tests: bool,
    limit: Option<usize>,
    compact: bool,
) -> Result<()> {
    let is_file_query = name.as_deref().is_some_and(looks_like_file_path);
    run_chunk_query(
        |c| {
            if let Some(ref n) = name {
                if is_file_query {
                    if !c.file.ends_with(n) && !c.file.ends_with(&n.replace('\\', "/")) { return false; }
                } else {
                    if !c.name.to_lowercase().contains(&n.to_lowercase()) { return false; }
                }
            }
            if let Some(ref k) = kind && c.kind.to_lowercase() != k.to_lowercase() { return false; }
            if !include_tests && rude_intel::graph::is_test_chunk(c) { return false; }
            true
        },
        "No symbols found.",
        |n| if is_file_query { format!("=== symbols: {n} in file ===\n") } else { format!("=== symbols: {n} found ===\n") },
        limit,
        compact,
    )?;
    if !is_file_query { print_trait_impls_if_relevant(name.as_deref())?; }
    Ok(())
}

fn looks_like_file_path(s: &str) -> bool {
    const EXTENSIONS: &[&str] = &[
        ".rs", ".go", ".py", ".js", ".ts", ".tsx", ".jsx",
        ".c", ".cpp", ".cc", ".h", ".hpp", ".java", ".kt", ".cs", ".rb", ".swift",
    ];
    EXTENSIONS.iter().any(|ext| s.ends_with(ext)) || s.contains('/')
}

fn print_trait_impls_if_relevant(name: Option<&str>) -> Result<()> {
    let Some(name) = name else { return Ok(()) };
    let Some(graph) = graph::CallGraph::load(crate::db()) else { return Ok(()) };
    for idx in graph.resolve(name) {
        let i = idx as usize;
        let impls = &graph.trait_impls[i];
        if graph.chunks[i].kind != "trait" || impls.is_empty() { continue; }
        println!("  implementations of {}:", graph.chunks[i].name);
        for ii in impls.iter().map(|&x| x as usize) {
            println!("    {}{}  [{}] {}", relative_path(&graph.chunks[ii].file),
                format_lines_opt(graph.chunks[ii].lines), graph.chunks[ii].kind, graph.chunks[ii].name);
        }
        println!();
    }
    Ok(())
}

pub fn run_context(
    symbol: String,
    depth: u32,
    source: bool,
    include_tests: bool,
    scope: Option<String>,
    tree: bool,
    blast: bool,
) -> Result<()> {
    if tree { return run_context_tree(&symbol, depth, include_tests); }
    if blast { return run_blast(&symbol, depth, include_tests, &scope); }

    use rude_intel::context_cmd;
    let graph = load_or_build_graph()?;
    let result = context_cmd::build_context(&graph, &symbol, depth);

    if result.seeds.is_empty() {
        println!("No symbol found matching \"{symbol}\".");
        return Ok(());
    }

    let seed0 = result.seeds.first().copied();
    let site = |a, b| seed0.map_or(0, |_| graph.call_site_line(a, b));
    let mut entries: Vec<TaggedEntry> = Vec::new();
    for &i in &result.seeds  { entries.push(TaggedEntry { idx: i, tag: "def",    sig: true,  call_line: 0 }); }
    for e in &result.callers { entries.push(TaggedEntry { idx: e.idx, tag: "caller", sig: false, call_line: site(e.idx, seed0.unwrap_or(0)) }); }
    for e in &result.callees { entries.push(TaggedEntry { idx: e.idx, tag: "callee", sig: false, call_line: site(seed0.unwrap_or(0), e.idx) }); }
    for &i in &result.types  { entries.push(TaggedEntry { idx: i, tag: "type",   sig: false, call_line: 0 }); }
    if include_tests {
        for &i in &result.tests { entries.push(TaggedEntry { idx: i, tag: "test", sig: false, call_line: site(i, seed0.unwrap_or(0)) }); }
    }

    if let Some(ref sc) = scope {
        entries.retain(|e| e.tag == "def" || {
            let f = &graph.chunks[e.idx as usize].file;
            f.starts_with(sc.as_str()) || f.contains(sc.as_str())
        });
    }

    let (alias_map, _) = graph.global_aliases();
    println!("=== context: {symbol}{} ({} caller, {} callee, {} type, {} test) ===\n",
        fmt_scope(&scope), result.callers.len(), result.callees.len(),
        result.types.len(), result.tests.len());
    print_tagged(&graph, &entries, source, &alias_map);

    if !include_tests && !result.tests.is_empty() {
        println!("  {} tests (use --include-tests to show)\n", result.tests.len());
    }

    // Field access summary for struct seeds
    if let Some(&si) = result.seeds.iter().find(|&&s| graph.chunks[s as usize].kind == "struct") {
        let field_entries = graph.find_field_accesses_for_type(&graph.chunks[si as usize].name.clone());
        if !field_entries.is_empty() {
            println!("@ [field accesses]");
            for (field, indices) in &field_entries {
                let names: Vec<&str> = indices.iter().map(|&i| graph.chunks[i as usize].name.as_str()).collect();
                println!("  .{field} ← {}", names.join(", "));
            }
            println!();
        }
    }
    Ok(())
}

fn run_context_tree(symbol: &str, depth: u32, include_tests: bool) -> Result<()> {
    use rude_intel::jump;
    let graph = load_or_build_graph()?;
    let Some(seeds) = resolve_symbol(&graph, symbol) else { return Ok(()) };
    let (alias_map, _) = graph.global_aliases();
    println!("=== jump: {symbol} ===\n");
    print!("{}", jump::render_tree(&graph, &jump::build_flow_tree(&graph, &seeds, depth, !include_tests), &alias_map));
    Ok(())
}

fn run_blast(symbol: &str, depth: u32, include_tests: bool, scope: &Option<String>) -> Result<()> {
    let effective_depth = if depth == 1 { 2 } else { depth };
    let graph = load_or_build_graph()?;
    let (alias_map, _) = graph.global_aliases();

    if let Some(dot) = symbol.find('.') {
        let key = format!("{}::{}", symbol[..dot].to_lowercase(), symbol[dot+1..].to_lowercase());
        let field_chunks = graph.find_field_access(&key);
        if field_chunks.is_empty() { println!("No field accesses found for {symbol}"); return Ok(()); }
        let all = impact::bfs_reverse(&graph, &field_chunks, effective_depth);
        let (prod, test) = count_prod_test(&all);
        print!("=== context: {symbol}{} ({} field accessors, {} affected, {} prod, {} test) ===\n",
            fmt_scope(scope), field_chunks.len(), prod + test, prod, test);
        let entries = tag_blast(all, &graph, scope, include_tests, Some(&field_chunks));
        print_tagged(&graph, &entries, false, &alias_map);
        return Ok(());
    }

    let Some(s) = resolve_symbol(&graph, symbol) else { return Ok(()) };
    let seeds = impact::expand_seeds_with_traits(&graph, &s);
    let all = impact::bfs_reverse(&graph, &seeds, effective_depth);
    let (prod, test) = count_prod_test(&all);
    print!("=== context: {symbol}{} ({} affected, {} prod, {} test) ===\n",
        fmt_scope(scope), prod + test, prod, test);
    let entries = tag_blast(all, &graph, scope, include_tests, None);
    print_tagged(&graph, &entries, false, &alias_map);
    Ok(())
}

fn count_prod_test(entries: &[impact::BfsEntry]) -> (usize, usize) {
    entries.iter().filter(|e| e.depth > 0)
        .fold((0, 0), |(p, t), e| if e.is_test { (p, t + 1) } else { (p + 1, t) })
}

fn tag_blast(
    all: Vec<impact::BfsEntry>,
    graph: &graph::CallGraph,
    scope: &Option<String>,
    include_tests: bool,
    field_chunks: Option<&[u32]>,
) -> Vec<TaggedEntry> {
    all.into_iter()
        .filter(|e| scope.as_ref().map_or(true, |prefix| {
            let f = &graph.chunks[e.idx as usize].file;
            f.starts_with(prefix.as_str()) || f.contains(prefix.as_str())
        }))
        .filter(|e| include_tests || !e.is_test)
        .map(|e| {
            let tag = field_chunks.map_or_else(
                || depth_tag(e.depth),
                |fc| if fc.contains(&e.idx) { "field" } else { depth_tag(e.depth) },
            );
            TaggedEntry { idx: e.idx, tag, sig: false, call_line: 0 }
        })
        .collect()
}

#[inline]
fn depth_tag(depth: u32) -> &'static str {
    match depth { 0 => "target", 1 => "d1", 2 => "d2", _ => "d3+" }
}

fn resolve_symbol(graph: &graph::CallGraph, symbol: &str) -> Option<Vec<u32>> {
    let seeds = graph.resolve(symbol);
    if seeds.is_empty() { println!("No symbol found matching \"{symbol}\"."); None } else { Some(seeds) }
}

pub fn run_trace(from: String, to: String) -> Result<()> {
    let graph = load_or_build_graph()?;
    let (alias_map, _) = graph.global_aliases();
    let Some(sources) = resolve_symbol(&graph, &from) else { return Ok(()) };
    let Some(targets) = resolve_symbol(&graph, &to) else { return Ok(()) };
    match trace::bfs_shortest_path(&graph, &sources, &targets) {
        Some(path) => {
            println!("=== trace: {from} \u{2192} {to} ({} hops) ===\n", path.len() - 1);
            for (step, &idx) in path.iter().enumerate() {
                let i = idx as usize;
                let short = apply_alias(relative_path(&graph.chunks[i].file), &alias_map);
                let test_marker = if graph.is_test[i] { " [test]" } else { "" };
                let (arrow, indent) = if step == 0 { ("  ", String::new()) } else { ("→ ", "  ".repeat(step)) };
                println!("  {indent}{arrow}{short}{}  {}{test_marker}", format_lines_opt(graph.chunks[i].lines), graph.chunks[i].name);
            }
            println!();
        }
        None => println!("No call path found from \"{from}\" to \"{to}\"."),
    }
    Ok(())
}

#[inline]
fn fmt_scope(scope: &Option<String>) -> String {
    scope.as_ref().map_or(String::new(), |s| format!(" (scope: {s})"))
}

fn run_chunk_query(
    filter: impl Fn(&ParsedChunk) -> bool,
    empty_msg: &str,
    header: impl FnOnce(usize) -> String,
    limit: Option<usize>,
    compact: bool,
) -> Result<()> {
    let graph = load_or_build_graph()?;
    let chunks = &graph.chunks;
    let all_files: Vec<&str> = chunks.iter().map(|c| relative_path(&c.file)).collect();
    let (alias_map, _) = build_path_aliases(&all_files);

    let filtered: Vec<&ParsedChunk> = chunks.iter().filter(|c| filter(c)).collect();
    let total = filtered.len();
    let display: Vec<&ParsedChunk> = if let Some(n) = limit { filtered.into_iter().take(n).collect() } else { filtered };

    if display.is_empty() {
        println!("{empty_msg}");
        return Ok(());
    }

    let suffix = limit.map_or(String::new(), |n| format!(" (showing {}/{})", display.len().min(n), total));
    println!("{}{suffix}", header(total));

    let files: Vec<&str> = display.iter().map(|c| relative_path(&c.file)).collect();
    let mut groups: BTreeMap<&str, Vec<&ParsedChunk>> = BTreeMap::new();
    for (c, file) in display.iter().zip(files.iter()) { groups.entry(file).or_default().push(c); }
    for (file, items) in &groups {
        println!("@ {}", apply_alias(file, &alias_map));
        for c in items {
            let kind_tag = if c.kind == "function" { String::new() } else { format!("[{}] ", c.kind) };
            let test_marker = if graph::is_test_chunk(c) { " [test]" } else { "" };
            println!("  {} {kind_tag}{}{test_marker}", format_lines_opt(c.lines), c.name);
            if !compact { if let Some(sig) = c.signature.as_deref().filter(|s| !s.is_empty()) { println!("    {sig}"); } }
        }
        if !compact { println!(); }
    }
    Ok(())
}

struct TaggedEntry { idx: u32, tag: &'static str, sig: bool, call_line: u32 }

/// File-grouped output for TaggedEntry slices (context/blast).
fn print_tagged(
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
            println!("  [{}] {} {kind_tag}{}{test_marker}{call_site}", e.tag, format_lines_opt(graph.chunks[i].lines), graph.chunks[i].name);
            if e.sig { if let Some(s) = &graph.chunks[i].signature { println!("    {s}"); } }
            if show_source && (e.tag == "def" || e.sig) {
                if let Some((start, end)) = graph.chunks[i].lines {
                    if let Ok(content) = std::fs::read_to_string(&graph.chunks[i].file) {
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
