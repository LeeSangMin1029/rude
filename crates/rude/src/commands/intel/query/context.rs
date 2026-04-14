use anyhow::{bail, Result};

use rude_intel::graph::build::CallGraph;
use super::common::{load_or_build_graph, resolve_symbol, fmt_scope, print_tagged, TaggedEntry, save_query_context, rank_by_recent, print_no_symbol_hint};
use super::blast::run_blast;

const LEGEND: &str = "# markers: [def]=definition [caller]/[callee]=edges  →:N=call-site-line  [test]=test fn  [d1]/[d2]=BFS depth (blast)  ↩=recursion (tree)";

fn maybe_print_legend() {
    if std::env::var_os("RUDE_LEGEND").is_some() {
        println!("{LEGEND}");
    }
}

fn disambiguate(graph: &CallGraph, idx: usize) -> String {
    let chunk = &graph.chunks[idx];
    let dn = chunk.dn();
    if dn.contains("::") {
        return dn.to_string();
    }
    if let Some(sig) = &chunk.signature {
        if let Some(start) = sig.find("impl ") {
            let after = &sig[start + 5..];
            if let Some(end) = after.find(">::") {
                let impl_type = after[..end].rsplit("::").next().unwrap_or(&after[..end]);
                let for_part = after[..end].rsplit(" for ").next().unwrap_or(impl_type);
                let for_short = for_part.rsplit("::").next().unwrap_or(for_part);
                return format!("{for_short}::{dn}");
            }
        }
    }
    let name = &chunk.name;
    if name.contains("::") {
        return rude_util::display_symbol_name(name);
    }
    dn.to_string()
}

pub fn run_context(
    symbol: String,
    depth: u32,
    source: bool,
    include_tests: bool,
    scope: Option<String>,
    tree: bool,
    blast: bool,
    summary: bool,
) -> Result<()> {
    if summary && (source || tree || blast) {
        bail!("--summary is exclusive; do not combine with --source / --tree / --blast");
    }
    if tree { return run_context_tree(&symbol, depth, include_tests); }
    if blast { return run_blast(&symbol, depth, include_tests, &scope); }

    use rude_intel::context_cmd;
    let graph = load_or_build_graph()?;
    let mut result = context_cmd::build_context(&graph, &symbol, depth);

    if result.seeds.is_empty() {
        print_no_symbol_hint(&graph, &symbol);
        return Ok(());
    }
    rank_by_recent(&graph, &mut result.seeds);
    if !result.impl_groups.is_empty() {
        let seed_order: std::collections::HashMap<u32, usize> = result.seeds.iter()
            .enumerate().map(|(i, &s)| (s, i)).collect();
        result.impl_groups.sort_by_key(|g| seed_order.get(&g.seed_idx).copied().unwrap_or(usize::MAX));
    }
    save_query_context(&graph, &result.seeds);

    if summary {
        return print_summary(&graph, &result, &symbol, &scope);
    }
    maybe_print_legend();

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
        let before = entries.len();
        entries.retain(|e| e.tag == "def" || graph.chunks[e.idx as usize].file.contains(sc.as_str()));
        let def_count = entries.iter().filter(|e| e.tag == "def").count();
        if before > def_count && entries.len() == def_count {
            println!("scope '{sc}' matched no files (tip: check with `rude aliases`)");
        }
    }

    let (alias_map, _) = graph.global_aliases();

    if !result.impl_groups.is_empty() {
        let groups = &result.impl_groups;
        let trait_label = groups.first()
            .filter(|g| !g.trait_name.is_empty())
            .map(|g| format!("{}::", g.trait_name))
            .unwrap_or_default();
        println!("=== context: {trait_label}{symbol} ({} impls) ===", groups.len());
        let caller_name = |idx: u32| disambiguate(&graph, idx as usize);
        let all_caller_names: Vec<std::collections::HashSet<String>> = groups.iter()
            .map(|g| g.callers.iter().map(|c| caller_name(c.idx)).collect())
            .collect();
        let shared_names: std::collections::HashSet<String> = {
            let threshold = (all_caller_names.len() + 1) / 2; // majority
            let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            for set in &all_caller_names {
                for name in set { *counts.entry(name.clone()).or_default() += 1; }
            }
            counts.into_iter().filter(|(_, c)| *c >= threshold && *c > 1).map(|(n, _)| n).collect()
        };
        if !shared_names.is_empty() {
            let mut by_method: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
            for name in &shared_names {
                if let Some((owner, method)) = name.rsplit_once("::") {
                    by_method.entry(method).or_default().push(owner);
                } else {
                    by_method.entry(name.as_str()).or_default();
                }
            }
            println!("\n  callers:");
            for (method, owners) in &by_method {
                if owners.is_empty() {
                    println!("    {method}");
                } else {
                    let mut sorted = owners.clone();
                    sorted.sort();
                    println!("    .{method} ({})", sorted.join(", "));
                }
            }
        }
        let impl_labels: Vec<String> = groups.iter()
            .map(|g| {
                let i = g.seed_idx as usize;
                let dn = &g.impl_name;
                if dn.contains("::") {
                    dn.rsplit("::").nth(1).unwrap_or(dn).to_string()
                } else {
                    disambiguate(&graph, i)
                        .rsplit("::").nth(1)
                        .unwrap_or_else(|| {
                            let file = rude_util::relative_path(&graph.chunks[i].file);
                            rude_util::apply_alias(file, &alias_map).leak()
                        })
                        .to_string()
                }
            })
            .collect();
        println!("\n  impls: {}", impl_labels.join(", "));
        let mut callee_to_impls: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for (gi, g) in groups.iter().enumerate() {
            let impl_short = &impl_labels[gi];
            for c in &g.callees {
                callee_to_impls.entry(caller_name(c.idx)).or_default().push(impl_short.clone());
            }
            let unique_callers: Vec<String> = g.callers.iter()
                .map(|c| caller_name(c.idx))
                .filter(|n| !shared_names.contains(n))
                .collect();
            if !unique_callers.is_empty() {
                println!("    {} ({impl_short} only)", unique_callers.join(", "));
            }
        }
        if !callee_to_impls.is_empty() {
            println!("  callees:");
            let mut by_method: std::collections::BTreeMap<String, Vec<(String, Vec<String>)>> = std::collections::BTreeMap::new();
            for (callee, impls) in &callee_to_impls {
                let method = callee.rsplit("::").next().unwrap_or(callee);
                let owner = callee.rsplit_once("::").map(|(o, _)| o.to_string()).unwrap_or_default();
                by_method.entry(method.to_string()).or_default().push((owner, impls.clone()));
            }
            for (method, entries) in &by_method {
                let owners: Vec<&str> = entries.iter()
                    .filter(|(o, _)| !o.is_empty())
                    .map(|(o, _)| o.as_str())
                    .collect();
                let all_impls: std::collections::HashSet<&str> = entries.iter()
                    .flat_map(|(_, impls)| impls.iter().map(|s| s.as_str()))
                    .collect();
                let impl_note = if all_impls.len() == impl_labels.len() {
                    String::new()
                } else {
                    let mut sorted: Vec<&str> = all_impls.into_iter().collect();
                    sorted.sort();
                    format!(" ({})", sorted.join(", "))
                };
                if owners.len() > 1 {
                    let mut sorted = owners;
                    sorted.sort();
                    println!("    .{method} ({}){impl_note}", sorted.join(", "));
                } else if owners.len() == 1 {
                    println!("    {}::{method}{impl_note}", owners[0]);
                } else {
                    println!("    {method}{impl_note}");
                }
            }
        }
        println!();
    } else {
        let is_type = result.seeds.iter().any(|&s|
            matches!(graph.chunks[s as usize].kind.as_str(), "struct" | "enum" | "trait"));
        if is_type {
            println!("=== context: {symbol}{} ===\n", fmt_scope(&scope));
        } else {
            println!("=== context: {symbol}{} ({} caller, {} callee, {} type, {} test) ===\n",
                fmt_scope(&scope), result.callers.len(), result.callees.len(),
                result.types.len(), result.tests.len());
        }
        print_tagged(&graph, &entries, source, &alias_map);
    }

    if !include_tests && !result.tests.is_empty() {
        println!("  {} tests (use --include-tests to show)\n", result.tests.len());
    }

    if let Some(&si) = result.seeds.iter().find(|&&s| {
        matches!(graph.chunks[s as usize].kind.as_str(), "struct" | "enum" | "trait")
    }) {
        let type_name = &graph.chunks[si as usize].name;
        let type_dn = graph.chunks[si as usize].dn();
        let type_base = type_dn.split('<').next().unwrap_or(type_dn);
        let methods: Vec<(usize, &str)> = graph.chunks.iter().enumerate()
            .filter(|(_, c)| {
                if c.kind != "function" { return false; }
                let cdn = c.dn();
                if let Some((owner, _)) = cdn.rsplit_once("::") {
                    let owner_base = owner.split('<').next().unwrap_or(owner);
                    owner_base == type_base
                } else {
                    c.name.contains(&format!("{type_name}::")) || c.name.contains(&format!("{type_dn}::"))
                }
            })
            .map(|(i, c)| {
                let method = c.dn().rsplit("::").next().unwrap_or(c.dn());
                (i, method)
            })
            .collect();
        if !methods.is_empty() {
            println!("  methods:");
            for (i, method) in &methods {
                let loc = rude_util::format_lines_opt(graph.chunks[*i].lines);
                let sig = graph.chunks[*i].signature.as_deref()
                    .filter(|s| !s.is_empty())
                    .map(|s| format!("  {}", rude_util::shorten_signature(s, 60)))
                    .unwrap_or_default();
                println!("    {loc} {method}{sig}");
            }
            println!();
        }
        let field_entries = graph.find_field_accesses_for_type(&type_name.clone());
        if !field_entries.is_empty() {
            let mut by_method: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
            for (field, indices) in &field_entries {
                for &idx in indices.iter() {
                    let name = graph.chunks[idx as usize].dn();
                    let method = name.rsplit("::").next().unwrap_or(name);
                    by_method.entry(field.as_ref()).or_default().push(method);
                }
            }
            println!("  fields:");
            for (field, accessors) in &by_method {
                let mut deduped = accessors.clone();
                deduped.sort();
                deduped.dedup();
                println!("    .{field} ← {}", deduped.join(", "));
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

fn print_summary(
    graph: &CallGraph,
    result: &rude_intel::context_cmd::ContextResult,
    symbol: &str,
    scope: &Option<String>,
) -> Result<()> {
    use rude_intel::context_cmd::ContextEntry;
    let (alias_map, _) = graph.global_aliases();
    let in_scope = |idx: u32| -> bool {
        scope.as_ref().is_none_or(|sc| graph.chunks[idx as usize].file.contains(sc.as_str()))
    };
    let filter_entries = |es: &[ContextEntry]| -> Vec<u32> {
        es.iter().map(|e| e.idx).filter(|&i| in_scope(i)).collect()
    };
    let filter_ids = |ids: &[u32]| -> Vec<u32> {
        ids.iter().copied().filter(|&i| in_scope(i)).collect()
    };
    maybe_print_legend();

    if !result.impl_groups.is_empty() {
        let groups = &result.impl_groups;
        let files: std::collections::BTreeSet<String> = groups.iter()
            .map(|g| rude_util::apply_alias(
                rude_util::relative_path(&graph.chunks[g.seed_idx as usize].file),
                &alias_map,
            ))
            .collect();
        let file_list: Vec<String> = files.into_iter().collect();
        println!("=== context: {symbol}{} (summary: {} impls in {}) ===",
            fmt_scope(scope), groups.len(), file_list.join(", "));
        for g in groups {
            let i = g.seed_idx as usize;
            let loc = rude_util::format_lines_opt(graph.chunks[i].lines);
            let file = rude_util::apply_alias(rude_util::relative_path(&graph.chunks[i].file), &alias_map);
            let sig = graph.chunks[i].signature.as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| rude_util::shorten_signature(s, 100))
                .unwrap_or_else(|| g.impl_name.clone());
            println!("  [{file}{loc}] {sig}");
        }
        if scope.is_some() && !groups.iter().any(|g| in_scope(g.seed_idx)) {
            if let Some(sc) = scope {
                println!("scope '{sc}' matched no files (tip: check with `rude aliases`)");
            }
        }
        return Ok(());
    }

    let callers_filtered = filter_entries(&result.callers);
    let callees_filtered = filter_entries(&result.callees);
    let tests_filtered = filter_ids(&result.tests);

    if let Some(sc) = scope {
        let had_any = !result.callers.is_empty() || !result.callees.is_empty();
        let kept_any = !callers_filtered.is_empty() || !callees_filtered.is_empty();
        if had_any && !kept_any {
            println!("scope '{sc}' matched no files (tip: check with `rude aliases`)");
        }
    }

    println!("=== context: {symbol}{} (summary: {} caller, {} callee, {} test) ===",
        fmt_scope(scope), callers_filtered.len(), callees_filtered.len(), tests_filtered.len());

    let seed_idx = result.seeds.first().copied().unwrap_or(0);
    let seed = &graph.chunks[seed_idx as usize];
    let def_loc = rude_util::format_lines_opt(seed.lines);
    let def_file = rude_util::apply_alias(rude_util::relative_path(&seed.file), &alias_map);
    let def_sig = seed.signature.as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| rude_util::shorten_signature(s, 100))
        .unwrap_or_else(|| seed.dn().to_string());
    println!("[def]  [{def_file}{def_loc}]  {def_sig}");

    let print_edges = |label: &str, ids: &[u32], is_caller: bool| {
        if ids.is_empty() { return; }
        println!("{label}:");
        let limit = 5;
        for &idx in ids.iter().take(limit) {
            let c = &graph.chunks[idx as usize];
            let file = rude_util::apply_alias(rude_util::relative_path(&c.file), &alias_map);
            let loc = rude_util::format_lines_opt(c.lines);
            let site = if is_caller {
                graph.call_site_line(idx, seed_idx)
            } else {
                graph.call_site_line(seed_idx, idx)
            };
            let arrow = if site > 0 { format!("  → :{site}") } else { String::new() };
            println!("  [{file}{loc}]  {}{arrow}", c.dn());
        }
        if ids.len() > limit {
            println!("  (... {} more)", ids.len() - limit);
        }
    };
    print_edges("callers", &callers_filtered, true);
    print_edges("callees", &callees_filtered, false);
    Ok(())
}
