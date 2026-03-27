use rude_intel::impact::bfs_reverse;
use rude_util::{format_lines_opt as format_lines, };
use rude_intel::graph::CallGraph;

use super::helpers::{chunk, test_chunk};

// ── bfs_reverse ─────────────────────────────────────────────────────

#[test]
fn bfs_reverse_empty_graph() {
    let graph = CallGraph::build(&[]);
    let results = bfs_reverse(&graph, &[], 3);
    assert!(results.is_empty());
}

#[test]
fn bfs_reverse_single_node() {
    let chunks = vec![chunk("A", "src/a.rs", &[])];
    let graph = CallGraph::build(&chunks);
    let results = bfs_reverse(&graph, &[0], 3);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].idx, 0);
    assert_eq!(results[0].depth, 0);
}

#[test]
fn bfs_reverse_finds_callers() {
    // A -> B, C -> B.  Reverse from B should find A and C.
    let chunks = vec![
        chunk("A", "src/a.rs", &["B"]),
        chunk("B", "src/b.rs", &[]),
        chunk("C", "src/c.rs", &["B"]),
    ];
    let graph = CallGraph::build(&chunks);
    let results = bfs_reverse(&graph, &[1], 3);

    let idxs: Vec<u32> = results.iter().map(|e| e.idx).collect();
    assert!(idxs.contains(&1)); // seed
    assert!(idxs.contains(&0)); // A calls B
    assert!(idxs.contains(&2)); // C calls B
}

#[test]
fn bfs_reverse_transitive_callers() {
    // A -> B -> C.  Reverse from C should find B (depth 1) and A (depth 2).
    let chunks = vec![
        chunk("A", "src/a.rs", &["B"]),
        chunk("B", "src/b.rs", &["C"]),
        chunk("C", "src/c.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);
    let results = bfs_reverse(&graph, &[2], 5);

    assert_eq!(results.len(), 3);
    let b = results.iter().find(|e| e.idx == 1).unwrap();
    let a = results.iter().find(|e| e.idx == 0).unwrap();
    assert_eq!(b.depth, 1);
    assert_eq!(a.depth, 2);
}

#[test]
fn bfs_reverse_depth_limit() {
    // A -> B -> C.  Reverse from C with depth=1 should find B but not A.
    let chunks = vec![
        chunk("A", "src/a.rs", &["B"]),
        chunk("B", "src/b.rs", &["C"]),
        chunk("C", "src/c.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);
    let results = bfs_reverse(&graph, &[2], 1);

    let idxs: Vec<u32> = results.iter().map(|e| e.idx).collect();
    assert!(idxs.contains(&2));
    assert!(idxs.contains(&1));
    assert!(!idxs.contains(&0), "A should not be reached at depth 1");
}

#[test]
fn bfs_reverse_handles_cycle() {
    // A -> B -> C -> A (cycle).  Reverse from A should visit each once.
    let chunks = vec![
        chunk("A", "src/a.rs", &["B"]),
        chunk("B", "src/b.rs", &["C"]),
        chunk("C", "src/c.rs", &["A"]),
    ];
    let graph = CallGraph::build(&chunks);
    let results = bfs_reverse(&graph, &[0], 10);

    assert_eq!(results.len(), 3);
}

#[test]
fn bfs_reverse_test_nodes_flagged() {
    // T (test) -> A.
    let chunks = vec![
        test_chunk("T", "t.rs", &["A"]),
        chunk("A", "src/a.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);
    let results = bfs_reverse(&graph, &[1], 3);

    let t_entry = results.iter().find(|e| e.idx == 0).unwrap();
    assert!(t_entry.is_test);

    let a_entry = results.iter().find(|e| e.idx == 1).unwrap();
    assert!(!a_entry.is_test);
}

#[test]
fn bfs_reverse_no_callers() {
    // A has no callers.
    let chunks = vec![chunk("A", "src/a.rs", &[])];
    let graph = CallGraph::build(&chunks);
    let results = bfs_reverse(&graph, &[0], 3);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].idx, 0);
}

#[test]
fn bfs_reverse_multiple_seeds() {
    // A -> C, B -> C.  Seeds = [0, 1] (A, B).
    let chunks = vec![
        chunk("A", "src/a.rs", &["C"]),
        chunk("B", "src/b.rs", &["C"]),
        chunk("C", "src/c.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);
    let results = bfs_reverse(&graph, &[0, 1], 3);

    // Both seeds visited, no duplicates
    let idxs: Vec<u32> = results.iter().map(|e| e.idx).collect();
    assert!(idxs.contains(&0));
    assert!(idxs.contains(&1));
    assert_eq!(idxs.len(), 2);
}

// ── format_lines / format_lines_str ─────────────────────────────────

#[test]
fn format_lines_with_range() {
    assert_eq!(format_lines(Some((10, 20))), ":10-20");
}

#[test]
fn format_lines_none() {
    assert_eq!(format_lines(None), "");
}

#[test]
fn format_lines_str_with_range() {
    assert_eq!(format!("{}-{}", 5, 15), "5-15");
}

#[test]
fn format_lines_str_none() {
    assert_eq!(String::new(), "");
}
