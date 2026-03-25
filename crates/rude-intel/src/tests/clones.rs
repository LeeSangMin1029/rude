//! Unit tests for `clones` — ranges_overlap, UnifiedDupePair::tag.

use crate::clones::{ranges_overlap, UnifiedDupePair};

// ── ranges_overlap ───────────────────────────────────────────────────

#[test]
fn ranges_overlap_full() {
    assert!(ranges_overlap(1, 10, 5, 15));
}

#[test]
fn ranges_overlap_touching() {
    assert!(ranges_overlap(1, 5, 5, 10));
}

#[test]
fn ranges_overlap_contained() {
    assert!(ranges_overlap(1, 20, 5, 10));
}

#[test]
fn ranges_no_overlap() {
    assert!(!ranges_overlap(1, 5, 6, 10));
}

#[test]
fn ranges_no_overlap_reversed() {
    assert!(!ranges_overlap(10, 20, 1, 5));
}

#[test]
fn ranges_single_point_overlap() {
    assert!(ranges_overlap(5, 5, 5, 5));
}

// ── UnifiedDupePair::tag ─────────────────────────────────────────────

#[test]
fn tag_ast_only() {
    let pair = UnifiedDupePair {
        idx_a: 1, idx_b: 2, score: 1.0, jaccard: 0.3, ast_match: true,
    };
    assert_eq!(pair.tag(), "AST");
}

#[test]
fn tag_token_only() {
    let pair = UnifiedDupePair {
        idx_a: 1, idx_b: 2, score: 0.8, jaccard: 0.7, ast_match: false,
    };
    assert_eq!(pair.tag(), "Token");
}

#[test]
fn tag_ast_plus_token() {
    let pair = UnifiedDupePair {
        idx_a: 1, idx_b: 2, score: 1.0, jaccard: 0.9, ast_match: true,
    };
    assert_eq!(pair.tag(), "AST+Token");
}

#[test]
fn tag_weak() {
    let pair = UnifiedDupePair {
        idx_a: 1, idx_b: 2, score: 0.2, jaccard: 0.1, ast_match: false,
    };
    assert_eq!(pair.tag(), "Weak");
}
