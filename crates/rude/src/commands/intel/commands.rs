//! CLI command handlers for code-intel subcommands.
//!
//! Each `run_*` function corresponds to a CLI subcommand (stats, symbols,
//! context, blast, trace, etc.). Pure analysis logic lives in `rude-intel`.

use anyhow::Result;

use super::print_grouped;
use super::{
    build_stats, load_chunks, load_or_build_graph,
    format_lines_opt,
    graph, impact, trace, ParsedChunk,
};

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
    let indices = graph.resolve(name);
    for idx in indices {
        let i = idx as usize;
        if graph.kinds[i] != "trait" {
            continue;
        }
        let impls = &graph.trait_impls[i];
        if impls.is_empty() {
            continue;
        }
        println!("  implementations of {}:", graph.names[i]);
        for &impl_idx in impls {
            let ii = impl_idx as usize;
            let file = super::relative_path(&graph.files[ii]);
            let lines = format_lines_opt(graph.lines[ii]);
            println!("    {file}{lines}  [{}] {}", graph.kinds[ii], graph.names[ii]);
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
            let (mut prod_count, mut test_count) = (0usize, 0usize);
            for e in &all_entries {
                if e.depth == 0 { continue; }
                if e.is_test { test_count += 1; } else { prod_count += 1; }
            }
            let mut entries = filter_scope_entries(all_entries, &graph, &scope);
            entries = filter_test_entries(entries, include_tests);
            let mut tagged: Vec<TaggedEntry> = Vec::new();
            for e in &entries {
                let tag = if field_chunks.contains(&e.idx) {
                    "field"
                } else {
                    match e.depth {
                        0 => "target",
                        1 => "d1",
                        2 => "d2",
                        _ => "d3+",
                    }
                };
                tagged.push(TaggedEntry { idx: e.idx, tag, sig: false, call_line: 0 });
            }
            let scope_label = scope.as_ref().map_or(String::new(), |s| format!(" (scope: {s})"));
            println!("=== context: {symbol}{scope_label} ({} field accessors, {} affected, {} prod, {} test) ===\n",
                field_chunks.len(), prod_count + test_count, prod_count, test_count);
            print_file_grouped(&graph, &tagged, false, &alias_map);
            return Ok(());
        }

        let Some(seeds) = resolve_symbol(&graph, &symbol) else { return Ok(()) };
        let seeds = impact::expand_seeds_with_traits(&graph, &seeds);
        let all_entries = impact::bfs_reverse(&graph, &seeds, effective_depth);
        let (mut prod_count, mut test_count) = (0usize, 0usize);
        for e in &all_entries {
            if e.depth == 0 { continue; }
            if e.is_test { test_count += 1; } else { prod_count += 1; }
        }
        let mut entries = filter_scope_entries(all_entries, &graph, &scope);
        entries = filter_test_entries(entries, include_tests);

        let mut tagged: Vec<TaggedEntry> = Vec::new();
        for e in &entries {
            let tag = match e.depth {
                0 => "target",
                1 => "d1",
                2 => "d2",
                _ => "d3+",
            };
            tagged.push(TaggedEntry { idx: e.idx, tag, sig: false, call_line: 0 });
        }

        let scope_label = scope.as_ref().map_or(String::new(), |s| format!(" (scope: {s})"));
        println!("=== context: {symbol}{scope_label} ({} affected, {} prod, {} test) ===\n",
            prod_count + test_count, prod_count, test_count);
        print_file_grouped(&graph, &tagged, false, &alias_map);
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
    let mut entries: Vec<TaggedEntry> = Vec::new();
    for &idx in &result.seeds {
        entries.push(TaggedEntry { idx, tag: "def", sig: true, call_line: 0 });
    }
    for e in &result.callers {
        // Caller calls the seed — look up call site line
        let cl = result.seeds.first().map_or(0, |&seed| graph.call_site_line(e.idx, seed));
        entries.push(TaggedEntry { idx: e.idx, tag: "caller", sig: false, call_line: cl });
    }
    for e in &result.callees {
        // Seed calls this callee — look up call site line from seed
        let cl = result.seeds.first().map_or(0, |&seed| graph.call_site_line(seed, e.idx));
        entries.push(TaggedEntry { idx: e.idx, tag: "callee", sig: false, call_line: cl });
    }
    for &idx in &result.types {
        entries.push(TaggedEntry { idx, tag: "type", sig: false, call_line: 0 });
    }
    if include_tests {
        for &idx in &result.tests {
            let cl = result.seeds.first().map_or(0, |&seed| graph.call_site_line(idx, seed));
            entries.push(TaggedEntry { idx, tag: "test", sig: false, call_line: cl });
        }
    }

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
    if let Some(ref scope) = scope {
        println!("=== context: {symbol} (scope: {scope}) ({counts}) ===\n");
    } else {
        println!("=== context: {symbol} ({counts}) ===\n");
    }
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
    let all_files: Vec<&str> = chunks.iter().map(|c| rude_intel::helpers::relative_path(&c.file)).collect();
    let (alias_map, _legend) = rude_intel::helpers::build_path_aliases(&all_files);

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
    use rude_intel::helpers::apply_alias;

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
        .map(|e| super::relative_path(&graph.files[e.idx as usize]))
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
    use rude_intel::helpers::apply_alias;

    for (step, &idx) in path.iter().enumerate() {
        let i = idx as usize;
        let file = super::relative_path(&graph.files[i]);
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


/// `rude dead` — find functions with no callers (dead code candidates).
pub fn run_dead(
    include_pub: bool,
    file_filter: Option<String>,
) -> Result<()> {
    use rude_intel::helpers::extract_crate_name;

    let graph = load_or_build_graph()?;
    let n = graph.names.len();
    let (alias_map, _) = graph.global_aliases();

    let mut dead: Vec<usize> = Vec::new();

    for i in 0..n {
        if graph.is_test[i] || graph.kinds[i] != "function" {
            continue;
        }
        if is_derive_generated(&graph.names[i]) {
            continue;
        }
        if let Some(ref filter) = file_filter {
            if !graph.files[i].contains(filter.as_str()) {
                continue;
            }
        }
        if !graph.callers[i].is_empty() {
            continue;
        }
        if !include_pub {
            let is_pub = graph.signatures[i]
                .as_deref()
                .is_some_and(|s| s.starts_with("pub ") || s.starts_with("pub(crate)"));
            if is_pub {
                continue;
            }
        }
        if graph.names[i].starts_with('<') && graph.names[i].contains(" as ") {
            continue;
        }
        let name = &graph.names[i];
        if name == "main" || name.ends_with("::main") || name.ends_with("::run") {
            continue;
        }
        let file = &graph.files[i];
        if !file.ends_with(".rs") {
            continue;
        }
        if name.contains("::check::assert_impl")
            || name.contains("::{closure#0}::check")
            || name.ends_with("::new") && graph.callees[i].is_empty()
        {
            continue;
        }

        dead.push(i);
    }

    let mut by_crate: std::collections::BTreeMap<String, Vec<usize>> = std::collections::BTreeMap::new();
    for &i in &dead {
        let crate_name = extract_crate_name(&graph.files[i]);
        by_crate.entry(crate_name).or_default().push(i);
    }

    println!("=== dead code: {} functions with no callers ===\n", dead.len());

    for (crate_name, indices) in &by_crate {
        println!("[{}] {} dead:", crate_name, indices.len());
        for &i in indices {
            let loc = format_lines_opt(graph.lines[i]);
            let rel = super::relative_path(&graph.files[i]);
            let short = rude_intel::helpers::apply_alias(rel, &alias_map);
            println!("  {short}{loc}  {}", graph.names[i]);
        }
        println!();
    }

    Ok(())
}

/// `rude coverage` — test coverage via `cargo llvm-cov` with call-graph supplement.
pub fn run_coverage(
    _file_filter: Option<String>,
    refresh: bool,
) -> Result<()> {
    use std::collections::BTreeMap;
    use rude_intel::helpers::extract_crate_name;

    let llvm_cov_result = run_llvm_cov(refresh);

    let Some(cov) = llvm_cov_result else {
        println!("cargo llvm-cov not available. Install with: cargo install cargo-llvm-cov");
        return Ok(());
    };

    println!("=== test coverage (cargo llvm-cov) ===\n");

    let mut crate_cov: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
    for fc in &cov.files {
        let crate_name = extract_crate_name(&fc.filename);
        let entry = crate_cov.entry(crate_name).or_default();
        entry.0 += fc.fn_total;
        entry.1 += fc.fn_covered;
        entry.2 += fc.line_total;
        entry.3 += fc.line_covered;
    }

    println!(
        "{:<28} {:>8} {:>8} {:>10} {:>8} {:>8} {:>10}",
        "crate", "prod_fn", "covered", "fn_cov", "lines", "ln_cov", "ln_%"
    );
    println!("{}", "-".repeat(84));

    for (name, (fn_t, fn_c, ln_t, ln_c)) in &crate_cov {
        let fn_pct = if *fn_t > 0 {
            format!("{:.1}%", *fn_c as f64 / *fn_t as f64 * 100.0)
        } else {
            "N/A".to_owned()
        };
        let ln_pct = if *ln_t > 0 {
            format!("{:.1}%", *ln_c as f64 / *ln_t as f64 * 100.0)
        } else {
            "N/A".to_owned()
        };
        println!(
            "{:<28} {:>8} {:>8} {:>10} {:>8} {:>8} {:>10}",
            name, fn_t, fn_c, fn_pct, ln_t, ln_c, ln_pct
        );
    }

    println!("{}", "-".repeat(84));
    println!(
        "{:<28} {:>8} {:>8} {:>10} {:>8} {:>8} {:>10}",
        "total",
        cov.fn_total,
        cov.fn_covered,
        format!("{:.1}%", cov.fn_percent),
        cov.line_total,
        cov.line_covered,
        format!("{:.1}%", cov.line_percent),
    );
    println!();

    Ok(())
}

// ── llvm-cov integration ─────────────────────────────────────────────────

struct LlvmCovResult {
    fn_total: usize,
    fn_covered: usize,
    fn_percent: f64,
    line_total: usize,
    line_covered: usize,
    line_percent: f64,
    files: Vec<LlvmFileCov>,
}

struct LlvmFileCov {
    filename: String,
    fn_total: usize,
    fn_covered: usize,
    line_total: usize,
    line_covered: usize,
}

fn run_llvm_cov(refresh: bool) -> Option<LlvmCovResult> {
    let db = crate::db();
    let cache_path = db.join("cache").join("llvm_cov.json");

    let raw_json: serde_json::Value = if !refresh && cache_path.exists() {
        eprintln!("  [coverage] using cached {}", cache_path.display());
        let bytes = std::fs::read(&cache_path).ok()?;
        serde_json::from_slice(&bytes).ok()?
    } else {
        let project_root = db.parent()?;
        eprintln!("  [coverage] running cargo llvm-cov --json ...");

        let output = std::process::Command::new("cargo")
            .arg("llvm-cov")
            .arg("--json")
            .arg("--ignore-run-fail")
            .current_dir(project_root)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;

        if let Some(parent) = cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cache_path, &output.stdout);
        eprintln!("  [coverage] cached to {}", cache_path.display());

        json
    };

    parse_llvm_cov_json(&raw_json)
}

fn parse_llvm_cov_json(json: &serde_json::Value) -> Option<LlvmCovResult> {
    let data = json.get("data")?.get(0)?;
    let totals = data.get("totals")?;

    let functions = totals.get("functions")?;
    let lines = totals.get("lines")?;

    let mut files = Vec::new();
    if let Some(file_array) = data.get("files").and_then(|f| f.as_array()) {
        for entry in file_array {
            let filename = entry.get("filename")?.as_str()?.to_owned();
            let summary = entry.get("summary")?;
            let f = summary.get("functions")?;
            let l = summary.get("lines")?;
            files.push(LlvmFileCov {
                filename,
                fn_total: f.get("count")?.as_u64()? as usize,
                fn_covered: f.get("covered")?.as_u64()? as usize,
                line_total: l.get("count")?.as_u64()? as usize,
                line_covered: l.get("covered")?.as_u64()? as usize,
            });
        }
    }

    Some(LlvmCovResult {
        fn_total: functions.get("count")?.as_u64()? as usize,
        fn_covered: functions.get("covered")?.as_u64()? as usize,
        fn_percent: functions.get("percent")?.as_f64()?,
        line_total: lines.get("count")?.as_u64()? as usize,
        line_covered: lines.get("covered")?.as_u64()? as usize,
        line_percent: lines.get("percent")?.as_f64()?,
        files,
    })
}

fn is_derive_generated(name: &str) -> bool {
    name.contains("::_serde::") || name.contains("::_::_serde::")
    || name.contains("as bincode::Encode>::encode")
    || name.contains("as bincode::Decode<")
    || name.contains("as bincode::BorrowDecode<")
    || name.contains("as clap::")
}
