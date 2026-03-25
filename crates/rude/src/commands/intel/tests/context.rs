//! Unit tests for `commands::intel::context` helper functions.

use rude_intel::helpers::{format_lines_opt as format_lines, };

// ---------------------------------------------------------------------------
// format_lines
// ---------------------------------------------------------------------------

#[test]
fn format_lines_some() {
    assert_eq!(format_lines(Some((10, 20))), ":10-20");
    assert_eq!(format_lines(Some((1, 1))), ":1-1");
}

#[test]
fn format_lines_none_returns_empty() {
    assert_eq!(format_lines(None), "");
}

// ---------------------------------------------------------------------------
// format_lines_str
// ---------------------------------------------------------------------------

#[test]
fn format_lines_str_some() {
    assert_eq!(format!("{}-{}", 5, 15), "5-15");
    assert_eq!(format!("{}-{}", 100, 200), "100-200");
}

#[test]
fn format_lines_str_none_returns_empty() {
    assert_eq!(String::new(), "");
}

// ---------------------------------------------------------------------------
// bfs_forward
// ---------------------------------------------------------------------------


