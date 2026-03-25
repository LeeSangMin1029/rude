//! Call graph tests — verifies name-based edge resolution and graph structure.
//!
//! After RA transition, call resolution is done by daemon's outgoing_calls.
//! build_full only does name → index mapping (exact match → short fallback).
//! These tests verify that mapping and graph structure, not call resolution.

use crate::graph::CallGraph;
use crate::parse::ParsedChunk;

// ── Helpers ─────────────────────────────────────────────────────────

/// Create a minimal ParsedChunk for testing.
fn chunk(name: &str, file: &str, calls: &[&str]) -> ParsedChunk {
    ParsedChunk {
        kind: if name.contains("::") && !name.contains(" for ") { "function" } else if name.starts_with("impl ") || name.contains(" for ") { "impl" } else { "function" }.to_owned(),
        name: name.to_owned(),
        file: file.to_owned(),
        lines: Some((1, 10)),
        calls: calls.iter().map(|s| s.to_string()).collect(),
        call_lines: calls.iter().enumerate().map(|(i, _)| i as u32 + 1).collect(),
        ..Default::default()
    }
}

fn struct_chunk(name: &str, file: &str) -> ParsedChunk {
    ParsedChunk {
        kind: "struct".to_owned(),
        name: name.to_owned(),
        file: file.to_owned(),
        lines: Some((1, 5)),
        ..Default::default()
    }
}

fn has_edge(graph: &CallGraph, caller: &str, callee: &str) -> bool {
    let caller_lower = caller.to_lowercase();
    let callee_lower = callee.to_lowercase();
    let caller_idx = graph.names.iter().position(|n| n.to_lowercase() == caller_lower);
    let callee_idx = graph.names.iter().position(|n| n.to_lowercase() == callee_lower);
    if let (Some(ci), Some(ti)) = (caller_idx, callee_idx) {
        graph.callees[ci].iter().any(|&t| t as usize == ti)
    } else {
        false
    }
}

// ── Exact name matching ─────────────────────────────────────────────

#[test]
fn direct_function_call() {
    let chunks = vec![
        chunk("caller", "src/lib.rs", &["callee"]),
        chunk("callee", "src/lib.rs", &[]),
    ];
    let g = CallGraph::build(&chunks);
    assert!(has_edge(&g, "caller", "callee"));
}

#[test]
fn qualified_function_call() {
    // RA returns resolved name "Bar::exec"
    let chunks = vec![
        chunk("run", "src/foo.rs", &["Bar::exec"]),
        struct_chunk("Bar", "src/bar.rs"),
        chunk("Bar::exec", "src/bar.rs", &[]),
    ];
    let g = CallGraph::build(&chunks);
    assert!(has_edge(&g, "run", "Bar::exec"));
}

#[test]
fn self_method_resolved_by_ra() {
    // RA resolves self.initialize → Engine::initialize
    let chunks = vec![
        struct_chunk("Engine", "src/lib.rs"),
        chunk("Engine::start", "src/lib.rs", &["Engine::initialize"]),
        chunk("Engine::initialize", "src/lib.rs", &[]),
    ];
    let g = CallGraph::build(&chunks);
    assert!(has_edge(&g, "Engine::start", "Engine::initialize"));
}

#[test]
fn short_name_fallback() {
    // Call uses short name, matched via last :: segment
    let chunks = vec![
        chunk("caller", "src/a.rs", &["helper"]),
        chunk("utils::helper", "src/b.rs", &[]),
    ];
    let g = CallGraph::build(&chunks);
    assert!(has_edge(&g, "caller", "utils::helper"));
}

#[test]
fn no_self_edge() {
    let chunks = vec![
        chunk("foo", "src/lib.rs", &["foo"]),
    ];
    let g = CallGraph::build(&chunks);
    assert!(g.callees[0].is_empty());
}

#[test]
fn dedup_edges() {
    let chunks = vec![
        chunk("caller", "src/lib.rs", &["callee", "callee"]),
        chunk("callee", "src/lib.rs", &[]),
    ];
    let g = CallGraph::build(&chunks);
    assert_eq!(g.callees[0].len(), 1);
}

#[test]
fn cross_file_resolution() {
    let chunks = vec![
        chunk("mod_a::process", "src/a.rs", &["mod_b::validate"]),
        chunk("mod_b::validate", "src/b.rs", &[]),
    ];
    let g = CallGraph::build(&chunks);
    assert!(has_edge(&g, "mod_a::process", "mod_b::validate"));
}

#[test]
fn type_ref_creates_edge() {
    let chunks = vec![
        chunk("process", "src/lib.rs", &[]),
        struct_chunk("Config", "src/lib.rs"),
    ];
    // Add type ref
    let mut chunks = chunks;
    chunks[0].types = vec!["Config".to_owned()];
    let g = CallGraph::build(&chunks);
    assert!(has_edge(&g, "process", "Config"));
}

// ── Graph structure ─────────────────────────────────────────────────

#[test]
fn callers_populated() {
    let chunks = vec![
        chunk("a", "src/lib.rs", &["b"]),
        chunk("b", "src/lib.rs", &[]),
    ];
    let g = CallGraph::build(&chunks);
    assert!(!g.callers[1].is_empty());
    assert_eq!(g.names[g.callers[1][0] as usize], "a");
}

#[test]
fn call_site_line() {
    let chunks = vec![
        chunk("caller", "src/lib.rs", &["callee"]),
        chunk("callee", "src/lib.rs", &[]),
    ];
    let g = CallGraph::build(&chunks);
    let line = g.call_site_line(0, 1);
    assert_eq!(line, 1); // first call_line
}

#[test]
fn is_test_detection() {
    let chunks = vec![
        chunk("run", "src/lib.rs", &[]),
        chunk("test_something", "src/tests/foo.rs", &[]),
        chunk("normal", "src/lib.rs", &[]),
    ];
    let g = CallGraph::build(&chunks);
    assert!(!g.is_test[0], "normal function");
    assert!(g.is_test[1], "/tests/ path");
    assert!(!g.is_test[2], "normal function");
}

#[test]
fn is_test_from_attribute() {
    let mut c = chunk("my_test", "src/lib.rs", &[]);
    c.is_test = true;
    let g = CallGraph::build(&[c]);
    assert!(g.is_test[0], "is_test flag from chunk");
}

#[test]
fn name_index_sorted() {
    let chunks = vec![
        chunk("z_func", "src/lib.rs", &[]),
        chunk("a_func", "src/lib.rs", &[]),
        chunk("m_func", "src/lib.rs", &[]),
    ];
    let g = CallGraph::build(&chunks);
    let names: Vec<&str> = g.name_index.iter().map(|(n, _)| n.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[test]
fn graph_len_and_is_empty() {
    let g = CallGraph::build(&[]);
    assert!(g.is_empty());
    assert_eq!(g.len(), 0);

    let g = CallGraph::build(&[chunk("a", "src/lib.rs", &[])]);
    assert!(!g.is_empty());
    assert_eq!(g.len(), 1);
}

#[test]
fn trait_impls_populated() {
    let chunks = vec![
        ParsedChunk {
            kind: "trait".to_owned(),
            name: "Search".to_owned(),
            file: "src/lib.rs".to_owned(),
            lines: Some((1, 5)),
            ..Default::default()
        },
        ParsedChunk {
            kind: "impl".to_owned(),
            name: "Search for Engine".to_owned(),
            file: "src/lib.rs".to_owned(),
            lines: Some((6, 10)),
            ..Default::default()
        },
    ];
    let g = CallGraph::build(&chunks);
    assert!(!g.trait_impls[0].is_empty());
}

// ── CallGraph.resolve ────────────────────────────────────────────────

#[test]
fn resolve_by_exact_name() {
    let g = CallGraph::build(&[chunk("foo::bar", "src/lib.rs", &[])]);
    assert_eq!(g.resolve("foo::bar").len(), 1);
}

#[test]
fn resolve_case_insensitive() {
    let g = CallGraph::build(&[chunk("Foo::Bar", "src/lib.rs", &[])]);
    assert!(!g.resolve("foo::bar").is_empty());
}

#[test]
fn resolve_suffix_fallback() {
    let g = CallGraph::build(&[chunk("mod::foo", "src/lib.rs", &[])]);
    let results = g.resolve("foo");
    assert!(!results.is_empty());
}

#[test]
fn resolve_nonexistent_returns_empty() {
    let g = CallGraph::build(&[chunk("foo", "src/lib.rs", &[])]);
    assert!(g.resolve("nonexistent").is_empty());
}

// ── Utility tests ────────────────────────────────────────────────────

use crate::index_tables::{extract_leaf_type, extract_generic_bounds, owning_type, is_test_path};

#[test]
fn leaf_type_simple() {
    assert_eq!(extract_leaf_type("string"), "string");
}

#[test]
fn leaf_type_reference() {
    assert_eq!(extract_leaf_type("&foo"), "foo");
}

#[test]
fn leaf_type_mut_ref() {
    assert_eq!(extract_leaf_type("&mut bar"), "bar");
}

#[test]
fn leaf_type_generic() {
    assert_eq!(extract_leaf_type("vec<item>"), "item"); // vec is a wrapper, unwraps to inner
}

#[test]
fn leaf_type_unwraps_option() {
    assert_eq!(extract_leaf_type("option<widget>"), "widget");
}

#[test]
fn leaf_type_unwraps_result() {
    assert_eq!(extract_leaf_type("result<foo, error>"), "foo");
}

#[test]
fn leaf_type_unwraps_box() {
    assert_eq!(extract_leaf_type("box<widget>"), "widget");
}

#[test]
fn leaf_type_nested_wrappers() {
    assert_eq!(extract_leaf_type("option<vec<item>>"), "vec");
}

#[test]
fn leaf_type_self() {
    assert_eq!(extract_leaf_type("Self"), "Self");
}

#[test]
fn leaf_type_lifetime() {
    assert_eq!(extract_leaf_type("&'a foo"), "foo");
}

#[test]
fn leaf_type_dyn_trait() {
    assert_eq!(extract_leaf_type("dyn search"), "search");
}

#[test]
fn leaf_type_impl_trait() {
    assert_eq!(extract_leaf_type("impl display"), "display");
}

#[test]
fn owning_type_qualified() {
    assert_eq!(owning_type("Foo::bar"), Some("foo".to_owned()));
}

#[test]
fn owning_type_no_qualifier() {
    assert_eq!(owning_type("bar"), None);
}

#[test]
fn owning_type_trait_impl() {
    assert_eq!(owning_type("Search for Engine::run"), Some("engine".to_owned()));
}

#[test]
fn generic_bounds_simple() {
    let bounds = extract_generic_bounds("fn foo<T: Search>(x: T)");
    assert!(bounds.iter().any(|(t, b)| t == "t" && b == "search"));
}

#[test]
fn generic_bounds_where_clause() {
    let bounds = extract_generic_bounds("fn foo<T>(x: T) where T: Display {");
    assert!(bounds.iter().any(|(t, b)| t == "t" && b == "display"));
}

// ── is_test_path ─────────────────────────────────────────────────────

#[test]
fn test_path_detection() {
    assert!(is_test_path("src/tests/foo.rs"));
    assert!(is_test_path("src/test/bar.rs"));
    assert!(is_test_path("parser_test.rs"));
    assert!(is_test_path("parser_test.go"));
    assert!(!is_test_path("src/test_helpers.rs"));
    assert!(!is_test_path("src/lib.rs"));
    assert!(!is_test_path("src/main.rs"));
}
