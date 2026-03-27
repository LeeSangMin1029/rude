use rude_intel::trace::bfs_shortest_path;
use rude_util::{format_lines_opt as format_lines, };
use rude_intel::graph::CallGraph;

use super::helpers::chunk;

// ── bfs_shortest_path ───────────────────────────────────────────────

#[test]
fn no_path_empty_graph() {
    let graph = CallGraph::build(&[]);
    let result = bfs_shortest_path(&graph, &[], &[]);
    assert!(result.is_none());
}

#[test]
fn no_path_disconnected() {
    // A and B exist but no edge between them.
    let chunks = vec![
        chunk("A", "src/a.rs", &[]),
        chunk("B", "src/b.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);
    let result = bfs_shortest_path(&graph, &[0], &[1]);
    assert!(result.is_none());
}

#[test]
fn direct_connection() {
    // A -> B
    let chunks = vec![
        chunk("A", "src/a.rs", &["B"]),
        chunk("B", "src/b.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);
    let path = bfs_shortest_path(&graph, &[0], &[1]).unwrap();

    assert_eq!(path, vec![0, 1]);
}

#[test]
fn source_is_target() {
    // Source and target are the same node.
    let chunks = vec![chunk("A", "src/a.rs", &[])];
    let graph = CallGraph::build(&chunks);
    let path = bfs_shortest_path(&graph, &[0], &[0]).unwrap();

    assert_eq!(path, vec![0]);
}

#[test]
fn multi_hop_path() {
    // A -> B -> C -> D
    let chunks = vec![
        chunk("A", "src/a.rs", &["B"]),
        chunk("B", "src/b.rs", &["C"]),
        chunk("C", "src/c.rs", &["D"]),
        chunk("D", "src/d.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);
    let path = bfs_shortest_path(&graph, &[0], &[3]).unwrap();

    assert_eq!(path, vec![0, 1, 2, 3]);
}

#[test]
fn shortest_path_among_alternatives() {
    // A -> B -> D (2 hops)
    // A -> C -> E -> D (3 hops)
    // BFS should find the shorter path.
    let chunks = vec![
        chunk("A", "src/a.rs", &["B", "C"]),
        chunk("B", "src/b.rs", &["D"]),
        chunk("C", "src/c.rs", &["E"]),
        chunk("D", "src/d.rs", &[]),
        chunk("E", "src/e.rs", &["D"]),
    ];
    let graph = CallGraph::build(&chunks);
    let path = bfs_shortest_path(&graph, &[0], &[3]).unwrap();

    assert_eq!(path.len(), 3, "should be 2 hops (3 nodes)");
    assert_eq!(path[0], 0);
    assert_eq!(*path.last().unwrap(), 3);
}

#[test]
fn cycle_does_not_hang() {
    // A -> B -> C -> A (cycle), find path A -> C
    // Bidirectional BFS: A's callers include C, so shortest is A -> C (1 hop via caller edge).
    let chunks = vec![
        chunk("A", "src/a.rs", &["B"]),
        chunk("B", "src/b.rs", &["C"]),
        chunk("C", "src/c.rs", &["A"]),
    ];
    let graph = CallGraph::build(&chunks);
    let path = bfs_shortest_path(&graph, &[0], &[2]).unwrap();

    assert_eq!(path.len(), 2, "bidirectional: 1 hop via caller edge");
    assert_eq!(path[0], 0);
    assert_eq!(*path.last().unwrap(), 2);
}

#[test]
fn reverse_direction_finds_path() {
    // A -> B.  Bidirectional BFS from B finds A via caller edge.
    let chunks = vec![
        chunk("A", "src/a.rs", &["B"]),
        chunk("B", "src/b.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);
    let path = bfs_shortest_path(&graph, &[1], &[0]).unwrap();
    assert_eq!(path, vec![1, 0]);
}

#[test]
fn truly_disconnected_no_path() {
    // A and B have no edges at all — even bidirectional BFS finds nothing.
    let chunks = vec![
        chunk("A", "src/a.rs", &[]),
        chunk("B", "src/b.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);
    let result = bfs_shortest_path(&graph, &[0], &[1]);
    assert!(result.is_none());
}

#[test]
fn multiple_sources() {
    // X -> D, Y -> D.  Sources = [X, Y], target = D.
    let chunks = vec![
        chunk("X", "src/x.rs", &["D"]),
        chunk("Y", "src/y.rs", &["D"]),
        chunk("D", "src/d.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);
    let path = bfs_shortest_path(&graph, &[0, 1], &[2]).unwrap();

    assert_eq!(path.len(), 2); // 1 hop
    assert_eq!(*path.last().unwrap(), 2);
}

#[test]
fn multiple_targets() {
    // A -> B, A -> C.  Source = A, targets = [B, C].  Should find one of them.
    let chunks = vec![
        chunk("A", "src/a.rs", &["B", "C"]),
        chunk("B", "src/b.rs", &[]),
        chunk("C", "src/c.rs", &[]),
    ];
    let graph = CallGraph::build(&chunks);
    let path = bfs_shortest_path(&graph, &[0], &[1, 2]).unwrap();

    assert_eq!(path.len(), 2);
    assert_eq!(path[0], 0);
    let target = *path.last().unwrap();
    assert!(target == 1 || target == 2);
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
