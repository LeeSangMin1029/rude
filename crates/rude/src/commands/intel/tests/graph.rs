use rude_intel::graph::CallGraph;

use super::helpers::chunk;

// ── CallGraph::build ─────────────────────────────────────────────────

#[test]
fn build_empty() {
    let graph = CallGraph::build(&[]);
    assert_eq!(graph.len(), 0);
    assert!(graph.is_empty());
}

#[test]
fn build_records_metadata() {
    let chunks = vec![chunk("Foo::bar", "src/foo.rs", &[])];
    let graph = CallGraph::build(&chunks);

    assert_eq!(graph.len(), 1);
    assert_eq!(graph.names[0], "Foo::bar");
    assert_eq!(graph.files[0], "src/foo.rs");
    assert_eq!(graph.kinds[0], "function");
    assert_eq!(graph.lines[0], Some((1, 10)));
    assert_eq!(graph.signatures[0].as_deref(), Some("fn Foo::bar()"));
}

#[test]
fn build_creates_callee_and_caller_edges() {
    let chunks = vec![
        chunk("A::run", "src/a.rs", &["B::exec"]),
        chunk("B::exec", "src/b.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);

    // A calls B
    assert_eq!(graph.callees[0], vec![1]);
    // B is called by A
    assert_eq!(graph.callers[1], vec![0]);
    // B calls nothing
    assert!(graph.callees[1].is_empty());
    // A has no callers
    assert!(graph.callers[0].is_empty());
}

#[test]
fn build_ignores_self_calls() {
    let chunks = vec![chunk("A::run", "src/a.rs", &["A::run"])];
    let graph = CallGraph::build(&chunks);

    assert!(graph.callees[0].is_empty());
    assert!(graph.callers[0].is_empty());
}

#[test]
fn build_deduplicates_edges() {
    let chunks = vec![
        chunk("A", "src/a.rs", &["B", "B", "B"]),
        chunk("B", "src/b.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);

    assert_eq!(graph.callees[0].len(), 1);
    assert_eq!(graph.callers[1].len(), 1);
}

#[test]
fn build_resolves_short_names() {
    let chunks = vec![
        chunk("mod_a::Alpha", "src/a.rs", &["Beta"]),
        chunk("mod_b::Beta", "src/b.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);

    // "Beta" matches mod_b::Beta via short name fallback
    assert_eq!(graph.callees[0], vec![1]);
}

// ── CallGraph::resolve ───────────────────────────────────────────────

#[test]
fn resolve_exact() {
    let chunks = vec![
        chunk("foo::bar", "src/foo.rs", &[]),
        chunk("baz::qux", "src/baz.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);

    let results = graph.resolve("foo::bar");
    assert_eq!(results, vec![0]);
}

#[test]
fn resolve_case_insensitive() {
    let chunks = vec![chunk("Foo::Bar", "src/foo.rs", &[])];
    let graph = CallGraph::build(&chunks);

    let results = graph.resolve("foo::bar");
    assert_eq!(results, vec![0]);
}

#[test]
fn resolve_suffix_fallback() {
    let chunks = vec![chunk("very::long::path::run", "src/a.rs", &[])];
    let graph = CallGraph::build(&chunks);

    // Exact match fails, should match via ::run suffix
    let results = graph.resolve("run");
    assert_eq!(results, vec![0]);
}

#[test]
fn resolve_no_match() {
    let chunks = vec![chunk("Foo::bar", "src/foo.rs", &[])];
    let graph = CallGraph::build(&chunks);

    let results = graph.resolve("nonexistent");
    assert!(results.is_empty());
}

// ── RA-resolved method calls ─────────────────────────────────────────

#[test]
fn build_resolves_method_calls_from_ra() {
    // RA resolves self.helper → Resolver::helper (via outgoing_calls)
    let chunks = vec![
        chunk("Resolver::helper", "src/lsp.rs", &[]),
        chunk("Resolver::do_work", "src/lsp.rs", &["Resolver::helper"]),
    ];
    let graph = CallGraph::build(&chunks);

    assert_eq!(graph.callees[1], vec![0]);
    assert_eq!(graph.callers[0], vec![1]);
}

// ── is_test detection ────────────────────────────────────────────────

#[test]
fn is_test_detection() {
    let chunks = vec![
        chunk("test_something", "src/lib.rs", &[]),          // name only, no #[test] attr
        chunk("run", "src/tests/foo.rs", &[]),                // /tests/ directory
        chunk("normal", "src/lib.rs", &[]),                   // normal function
        chunk("also_test", "src/test_helpers.rs", &[]),       // test_ in filename, not a test
    ];
    let graph = CallGraph::build(&chunks);

    assert!(!graph.is_test[0], "name prefix alone is not a test");
    assert!(graph.is_test[1], "/tests/ in path");
    assert!(!graph.is_test[2], "normal function");
    assert!(!graph.is_test[3], "test_ in filename is not a test");
}

// ── name_index sorted ────────────────────────────────────────────────

#[test]
fn name_index_is_sorted() {
    let chunks = vec![
        chunk("Zebra", "src/z.rs", &[]),
        chunk("Alpha", "src/a.rs", &[]),
        chunk("Middle", "src/m.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);

    let names: Vec<&str> = graph.name_index.iter().map(|(n, _)| n.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}
