//! Query command handlers — stats, symbols, context, trace, aliases.
//!
//! Each `run_*` function corresponds to a CLI subcommand. Pure analysis
//! logic lives in `rude-intel`.

use std::collections::BTreeMap;

use anyhow::Result;

use rude_intel::graph;
use rude_intel::helpers::{apply_alias, build_path_aliases, format_lines_opt, relative_path};
use rude_intel::impact;
use rude_intel::loader::load_chunks;
use rude_intel::parse::ParsedChunk;
use rude_intel::stats::build_stats;
use rude_intel::trace;

pub(crate) fn load_or_build_graph() -> Result<graph::CallGraph> {
    rude_intel::loader::load_or_build_graph(crate::db())
}

pub(crate) fn load_or_build_graph_with_chunks() -> Result<(graph::CallGraph, Option<Vec<ParsedChunk>>)> {
    rude_intel::loader::load_or_build_graph_with_chunks(crate::db())
}

// ── Commands ─────────────────────────────────────────────────────────────

/// `rude aliases` — print global path alias mapping.
pub fn run_aliases() -> Result<()> {
    let graph = load_or_build_graph()?;
    let (_alias_map, legend) = graph.global_aliases();
    for (alias, dir) in &legend {
        println!("{alias} = {dir}");
    }
    Ok(())
}

/// `rude stats` — per-crate summary of code symbols.
pub fn run_stats() -> Result<()> {
    let chunks = load_chunks(crate::db())?;
    let stats = build_stats(&chunks);
    println!("=== stats: {} crates ===\n", stats.len());
    println!(
        "{:<24} {:>8} {:>8} {:>8} {:>8}",
        "crate", "prod_fn", "test_fn", "struct", "enum"
    );
    println!("{}", "-".repeat(60));
    let mut totals = [0usize; 4];
    for (name, row) in &stats {
        println!(
            "{:<24} {:>8} {:>8} {:>8} {:>8}",
            name, row[0], row[1], row[2], row[3]
        );
        for (i, v) in row.iter().enumerate() {
            totals[i] += v;
        }
    }
    println!("{}", "-".repeat(60));
    println!(
        "{:<24} {:>8} {:>8} {:>8} {:>8}",
        "total", totals[0], totals[1], totals[2], totals[3]
    );
    Ok(())
}

/// `rude symbols` — list symbols matching filters.
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
                    // File mode: match file path suffix
                    if !c.file.ends_with(n) && !c.file.ends_with(&n.replace('\\', "/")) {
                        return false;
                    }
                } else {
                    // Name mode: substring match
                    if !c.name.to_lowercase().contains(&n.to_lowercase()) { return false; }
                }
            }
            if let Some(ref k) = kind
                && c.kind.to_lowercase() != k.to_lowercase() { return false; }
            if !include_tests && rude_intel::graph::is_test_chunk(c) { return false; }
            true
        },
        "No symbols found.",
        |n| {
            if is_file_query {
                format!("=== symbols: {n} in file ===\n")
            } else {
                format!("=== symbols: {n} found ===\n")
            }
        },
        limit,
        compact,
    )?;

    // Show trait implementations if any trait was in the results (name mode only).
    if !is_file_query {
        print_trait_impls_if_relevant(name.as_deref())?;
    }
    Ok(())
}

/// Check if a string looks like a file path rather than a symbol name.
///
/// Heuristic: contains a known source file extension or path separator.
fn looks_like_file_path(s: &str) -> bool {
    const EXTENSIONS: &[&str] = &[
        ".rs", ".go", ".py", ".js", ".ts", ".tsx", ".jsx",
        ".c", ".cpp", ".cc", ".h", ".hpp",
        ".java", ".kt", ".cs", ".rb", ".swift",
    ];
    EXTENSIONS.iter().any(|ext| s.ends_with(ext)) || s.contains('/')
}

/// If the symbol search matched any trait, print its implementations.
fn print_trait_impls_if_relevant(name: Option<&str>) -> Result<()> {
    let name = match name {
        Some(n) => n,
        None => return Ok(()),
    };
    // Use cached graph only — don't trigger a full build just for trait impls.
    let graph = match graph::CallGraph::load(crate::db()) {
        Some(g) => g,
        None => return Ok(()),
    };
    for idx in graph.resolve(name) {
        let i = idx as usize;
        let impls = &graph.trait_impls[i];
        if graph.kinds[i] != "trait" || impls.is_empty() { continue; }
        println!("  implementations of {}:", graph.names[i]);
        for ii in impls.iter().map(|&x| x as usize) {
            println!("    {}{}  [{}] {}",
                relative_path(&graph.files[ii]),
                format_lines_opt(graph.lines[ii]),
                graph.kinds[ii], graph.names[ii]);
        }
        println!();
    }
    Ok(())
}

/// `rude context <symbol> --depth N`
pub fn run_context(
    symbol: String,
    depth: u32,
    source: bool,
    include_tests: bool,
    scope: Option<String>,
    tree: bool,
    blast: bool,
) -> Result<()> {
    if tree {
        return run_context_tree(&symbol, depth, include_tests);
    }

    if blast {
        let effective_depth = if depth == 1 { 2 } else { depth };

        let graph = load_or_build_graph()?;
        let (alias_map, _) = graph.global_aliases();

        // Field-level blast: "Type.field" notation
        if let Some(dot) = symbol.find('.') {
            let type_name = &symbol[..dot];
            let field_name = &symbol[dot + 1..];
            let key = format!("{}::{}", type_name.to_lowercase(), field_name.to_lowercase());
            let field_chunks = graph.find_field_access(&key);
            if field_chunks.is_empty() {
                println!("No field accesses found for {symbol}");
                return Ok(());
            }
            let all_entries = impact::bfs_reverse(&graph, &field_chunks, effective_depth);
            let (prod_count, test_count) = count_prod_test(&all_entries);
            let header = format!("=== context: {symbol}{} ({} field accessors, {} affected, {} prod, {} test) ===\n",
                fmt_scope(&scope), field_chunks.len(), prod_count + test_count, prod_count, test_count);
            print_blast_result(all_entries, &graph, &scope, include_tests, Some(&field_chunks), &header, &alias_map);
            return Ok(());
        }

        let Some(seeds) = resolve_symbol(&graph, &symbol) else { return Ok(()) };
        let seeds = impact::expand_seeds_with_traits(&graph, &seeds);
        let all_entries = impact::bfs_reverse(&graph, &seeds, effective_depth);
        let (prod_count, test_count) = count_prod_test(&all_entries);
        let header = format!("=== context: {symbol}{} ({} affected, {} prod, {} test) ===\n",
            fmt_scope(&scope), prod_count + test_count, prod_count, test_count);
        print_blast_result(all_entries, &graph, &scope, include_tests, None, &header, &alias_map);
        return Ok(());
    }

    use rude_intel::context_cmd;

    let chunks = load_chunks(crate::db())?;
    let graph = load_or_build_graph()?;
    let result = context_cmd::build_context(&graph, &chunks, &symbol, depth);

    if result.seeds.is_empty() {
        println!("No symbol found matching \"{symbol}\".");
        return Ok(());
    }

    // Build tagged entries for file-grouped output
    let mut entries = build_context_entries(&result, &graph, include_tests);

    // Apply scope filter: keep seeds (def) always, filter others by file path
    if let Some(ref scope) = scope {
        entries.retain(|e| {
            e.tag == "def" || {
                let file = &graph.files[e.idx as usize];
                file.starts_with(scope.as_str()) || file.contains(scope.as_str())
            }
        });
    }

    // Header with counts
    let counts = format!(
        "{} caller, {} callee, {} type, {} test",
        result.callers.len(), result.callees.len(),
        result.types.len(), result.tests.len(),
    );
    let (alias_map, _) = graph.global_aliases();
    println!("=== context: {symbol}{} ({counts}) ===\n", fmt_scope(&scope));
    print_file_grouped(&graph, &entries, source, &alias_map);

    if !include_tests && !result.tests.is_empty() {
        println!("  {} tests (use --include-tests to show)\n", result.tests.len());
    }

    // Show field access summary for struct seeds.
    let is_struct_seed = result.seeds.iter().any(|&s| graph.kinds[s as usize] == "struct");
    if is_struct_seed {
        // Use the struct name (lowercase) to look up field accesses
        let type_name = result.seeds.iter()
            .find(|&&s| graph.kinds[s as usize] == "struct")
            .map(|&s| &graph.names[s as usize]);
        if let Some(tn) = type_name {
            let field_entries = graph.find_field_accesses_for_type(tn);
            if !field_entries.is_empty() {
                println!("@ [field accesses]");
                for (field, indices) in &field_entries {
                    let accessors: Vec<&str> = indices.iter()
                        .map(|&i| graph.names[i as usize].as_str())
                        .collect();
                    println!("  .{field} ← {}", accessors.join(", "));
                }
                println!();
            }
        }
    }

    Ok(())
}


/// Shared tree-mode implementation used by both `context --tree` and `jump`.
fn run_context_tree(symbol: &str, depth: u32, include_tests: bool) -> Result<()> {
    use rude_intel::jump;

    let graph = load_or_build_graph()?;
    let Some(seeds) = resolve_symbol(&graph, symbol) else { return Ok(()) };
    let (alias_map, _legend) = graph.global_aliases();

    println!("=== jump: {symbol} ===\n");
    let skip_test = !include_tests;
    let tree = jump::build_flow_tree(&graph, &seeds, depth, skip_test);
    print!("{}", jump::render_tree(&graph, &tree, &alias_map));

    Ok(())
}


/// Common blast output: filter → build tagged → print header → print file-grouped.
fn print_blast_result(
    all_entries: Vec<impact::BfsEntry>,
    graph: &graph::CallGraph,
    scope: &Option<String>,
    include_tests: bool,
    field_chunks: Option<&[u32]>,
    header: &str,
    alias_map: &std::collections::BTreeMap<String, String>,
) {
    let mut entries = filter_scope_entries(all_entries, graph, scope);
    entries = filter_test_entries(entries, include_tests);
    let tagged = build_blast_tagged(&entries, field_chunks);
    print!("{header}");
    print_file_grouped(graph, &tagged, false, alias_map);
}

/// Build `TaggedEntry` list from a `ContextResult` for normal (non-blast) context output.
fn build_context_entries(
    result: &rude_intel::context_cmd::ContextResult,
    graph: &graph::CallGraph,
    include_tests: bool,
) -> Vec<TaggedEntry> {
    let seed0 = result.seeds.first().copied();
    let mut entries: Vec<TaggedEntry> = Vec::new();
    for &idx in &result.seeds {
        entries.push(TaggedEntry { idx, tag: "def", sig: true, call_line: 0 });
    }
    for e in &result.callers {
        let cl = seed0.map_or(0, |seed| graph.call_site_line(e.idx, seed));
        entries.push(TaggedEntry { idx: e.idx, tag: "caller", sig: false, call_line: cl });
    }
    for e in &result.callees {
        let cl = seed0.map_or(0, |seed| graph.call_site_line(seed, e.idx));
        entries.push(TaggedEntry { idx: e.idx, tag: "callee", sig: false, call_line: cl });
    }
    for &idx in &result.types {
        entries.push(TaggedEntry { idx, tag: "type", sig: false, call_line: 0 });
    }
    if include_tests {
        for &idx in &result.tests {
            let cl = seed0.map_or(0, |seed| graph.call_site_line(idx, seed));
            entries.push(TaggedEntry { idx, tag: "test", sig: false, call_line: cl });
        }
    }
    entries
}

/// Count prod/test callers in `all_entries`, skipping depth-0 seeds.
fn count_prod_test(all_entries: &[impact::BfsEntry]) -> (usize, usize) {
    let mut prod = 0usize;
    let mut test = 0usize;
    for e in all_entries {
        if e.depth == 0 { continue; }
        if e.is_test { test += 1; } else { prod += 1; }
    }
    (prod, test)
}

/// Build `TaggedEntry` list for blast output.
/// When `field_chunks` is `Some`, entries whose `idx` is in the set get tag `"field"`.
fn build_blast_tagged(entries: &[impact::BfsEntry], field_chunks: Option<&[u32]>) -> Vec<TaggedEntry> {
    entries.iter().map(|e| {
        let tag = if let Some(fc) = field_chunks {
            if fc.contains(&e.idx) { "field" } else { depth_tag(e.depth) }
        } else {
            depth_tag(e.depth)
        };
        TaggedEntry { idx: e.idx, tag, sig: false, call_line: 0 }
    }).collect()
}

#[inline]
fn depth_tag(depth: u32) -> &'static str {
    match depth {
        0 => "target",
        1 => "d1",
        2 => "d2",
        _ => "d3+",
    }
}

/// Resolve a symbol name to graph indices, printing a message if not found.
/// Returns `None` (and prints) when resolution yields no results.
fn resolve_symbol(graph: &graph::CallGraph, symbol: &str) -> Option<Vec<u32>> {
    let seeds = graph.resolve(symbol);
    if seeds.is_empty() {
        println!("No symbol found matching \"{symbol}\".");
        None
    } else {
        Some(seeds)
    }
}

fn filter_test_entries(entries: Vec<impact::BfsEntry>, include_tests: bool) -> Vec<impact::BfsEntry> {
    if include_tests {
        entries
    } else {
        entries.into_iter().filter(|e| !e.is_test).collect()
    }
}

fn filter_scope_entries(
    entries: Vec<impact::BfsEntry>,
    graph: &graph::CallGraph,
    scope: &Option<String>,
) -> Vec<impact::BfsEntry> {
    if let Some(ref prefix) = *scope {
        entries
            .into_iter()
            .filter(|e| {
                let file = &graph.files[e.idx as usize];
                file.starts_with(prefix.as_str()) || file.contains(prefix.as_str())
            })
            .collect()
    } else {
        entries
    }
}

/// `rude trace <from> <to>`
pub fn run_trace(
    from: String,
    to: String,
) -> Result<()> {
    let graph = load_or_build_graph()?;
    let (alias_map, _) = graph.global_aliases();
    let Some(sources) = resolve_symbol(&graph, &from) else { return Ok(()) };
    let Some(targets) = resolve_symbol(&graph, &to) else { return Ok(()) };

    match trace::bfs_shortest_path(&graph, &sources, &targets) {
        Some(path) => {
            println!("=== trace: {from} \u{2192} {to} ({} hops) ===\n", path.len() - 1);
            print_trace_path(&graph, &path, &alias_map);
        }
        None => {
            println!("No call path found from \"{from}\" to \"{to}\".");
        }
    }

    Ok(())
}

/// Format an optional scope label for header output.
#[inline]
fn fmt_scope(scope: &Option<String>) -> String {
    scope.as_ref().map_or(String::new(), |s| format!(" (scope: {s})"))
}

// ── Internal helpers ─────────────────────────────────────────────────────

/// Shared runner for chunk-filter commands (symbols).
fn run_chunk_query(
    filter: impl Fn(&ParsedChunk) -> bool,
    empty_msg: &str,
    header: impl FnOnce(usize) -> String,
    limit: Option<usize>,
    compact: bool,
) -> Result<()> {
    let chunks = load_chunks(crate::db())?;
    // Compute aliases from ALL chunks (not just filtered) for global consistency.
    let all_files: Vec<&str> = chunks.iter().map(|c| relative_path(&c.file)).collect();
    let (alias_map, _legend) = build_path_aliases(&all_files);

    let filtered: Vec<&ParsedChunk> = chunks.iter().filter(|c| filter(c)).collect();
    let total = filtered.len();
    let display: Vec<&ParsedChunk> = if let Some(n) = limit {
        filtered.into_iter().take(n).collect()
    } else {
        filtered
    };
    if display.is_empty() {
        println!("{empty_msg}");
    } else {
        let suffix = if let Some(n) = limit { format!(" (showing {}/{})", display.len().min(n), total) } else { String::new() };
        println!("{}{suffix}", header(total));
        print_grouped(&display, compact, &alias_map);
    }
    Ok(())
}

// ── File-grouped output (shared by context, blast) ──────────────────────

/// A tagged graph entry: index + role tag + whether to show signature.
struct TaggedEntry {
    idx: u32,
    tag: &'static str,
    sig: bool,
    /// Source line where this entry calls/is-called-by the seed (0 = unknown).
    call_line: u32,
}

/// Print entries grouped by file, with multi-base path aliases.
fn print_file_grouped(
    graph: &graph::CallGraph,
    entries: &[TaggedEntry],
    show_source: bool,
    alias_map: &std::collections::BTreeMap<String, String>,
) {
    use std::collections::BTreeMap;

    if entries.is_empty() {
        return;
    }

    // Deduplicate: if same idx appears in multiple roles, keep highest priority.
    let role_priority = |tag: &str| -> u8 {
        match tag {
            "def" => 0, "target" | "field" => 0,
            "caller" | "d1" | "d2" | "d3+" => 1,
            "callee" => 2,
            "type" => 3,
            "test" => 4,
            _ => 5,
        }
    };
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut deduped: Vec<&TaggedEntry> = Vec::new();
    let mut all_entries: Vec<&TaggedEntry> = entries.iter().collect();
    all_entries.sort_by_key(|e| role_priority(e.tag));
    for e in &all_entries {
        if seen.insert(e.idx) {
            deduped.push(e);
        }
    }

    let files: Vec<&str> = deduped
        .iter()
        .map(|e| relative_path(&graph.files[e.idx as usize]))
        .collect();
    let mut groups: BTreeMap<&str, Vec<&TaggedEntry>> = BTreeMap::new();
    for (entry, file) in deduped.iter().zip(files.iter()) {
        groups.entry(file).or_default().push(entry);
    }

    for (file, items) in &groups {
        let short = apply_alias(file, alias_map);
        println!("@ {short}");
        for e in items {
            let i = e.idx as usize;
            let lines = format_lines_opt(graph.lines[i]);
            let kind = &graph.kinds[i];
            let name = &graph.names[i];
            let test_marker = if graph.is_test[i] { " [test]" } else { "" };
            let call_site = if e.call_line > 0 {
                format!(" → :{}", e.call_line)
            } else {
                String::new()
            };
            let kind_tag = if *kind == "function" { String::new() } else { format!("[{kind}] ") };
            println!("  [{}] {lines} {kind_tag}{name}{test_marker}{call_site}", e.tag);
            if e.sig
                && let Some(s) = &graph.signatures[i] {
                    println!("    {s}");
                }
            if show_source && (e.tag == "def" || e.sig)
                && let Some((start, end)) = graph.lines[i] {
                    print_source_lines(&graph.files[i], start, end);
                }
        }
        println!();
    }
}

/// Read and print source lines from a file.
fn print_source_lines(file_path: &str, start: usize, end: usize) {
    let Ok(content) = std::fs::read_to_string(file_path) else {
        return;
    };
    let lines: Vec<&str> = content.lines().collect();
    let start = start.saturating_sub(1);
    let end = end.min(lines.len());
    if start >= end {
        return;
    }
    println!("    ```");
    for (i, line) in lines[start..end].iter().enumerate() {
        println!("    {:>4}│ {line}", start + i + 1);
    }
    println!("    ```");
}

fn print_trace_path(
    graph: &graph::CallGraph,
    path: &[u32],
    alias_map: &std::collections::BTreeMap<String, String>,
) {

    for (step, &idx) in path.iter().enumerate() {
        let i = idx as usize;
        let file = relative_path(&graph.files[i]);
        let short_file = apply_alias(file, alias_map);
        let name = &graph.names[i];
        let lines = format_lines_opt(graph.lines[i]);

        let test_marker = if graph.is_test[i] { " [test]" } else { "" };
        let arrow = if step == 0 { "  " } else { "→ " };
        let indent = if step == 0 { "" } else { &"  ".repeat(step) };
        println!("  {indent}{arrow}{short_file}{lines}  {name}{test_marker}");
    }
    println!();
}


/// Print chunks grouped by file with path aliases.
fn print_grouped(
    chunks: &[&ParsedChunk],
    compact: bool,
    alias_map: &BTreeMap<String, String>,
) {
    let files: Vec<&str> = chunks.iter().map(|c| relative_path(&c.file)).collect();

    let mut groups: BTreeMap<&str, Vec<&ParsedChunk>> = BTreeMap::new();
    for (c, file) in chunks.iter().zip(files.iter()) {
        groups.entry(file).or_default().push(c);
    }

    for (file, items) in &groups {
        let short = apply_alias(file, alias_map);
        println!("@ {short}");
        for c in items {
            let lines = format_lines_opt(c.lines);
            let test_marker = if graph::is_test_chunk(c) { " [test]" } else { "" };
            let kind_tag = if c.kind == "function" { String::new() } else { format!("[{}] ", c.kind) };
            println!("  {lines} {kind_tag}{name}{test_marker}", name = c.name);
            if !compact {
                let sig = c.signature.as_deref().unwrap_or("");
                if !sig.is_empty() {
                    println!("    {sig}");
                }
            }
        }
        if !compact { println!(); }
    }
}
