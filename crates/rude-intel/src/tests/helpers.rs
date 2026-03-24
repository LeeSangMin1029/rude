//! Unit tests for `helpers` module — formatting, path utils, JSON grouping.

use crate::helpers::{
    apply_alias, build_path_aliases, extract_crate_name, format_lines_opt, format_lines_str_opt,
    lines_str, relative_path,
};
use crate::parse::ParsedChunk;

// ── format_lines_str_opt ─────────────────────────────────────────────

#[test]
fn format_lines_str_opt_some() {
    assert_eq!(format_lines_str_opt(Some((10, 20))), "10-20");
}

#[test]
fn format_lines_str_opt_none() {
    assert_eq!(format_lines_str_opt(None), "");
}

// ── format_lines_opt ─────────────────────────────────────────────────

#[test]
fn format_lines_opt_some() {
    assert_eq!(format_lines_opt(Some((5, 15))), ":5-15");
}

#[test]
fn format_lines_opt_none() {
    assert_eq!(format_lines_opt(None), "");
}

// ── relative_path ────────────────────────────────────────────────────

#[test]
fn relative_path_strips_crates_prefix() {
    assert_eq!(
        relative_path("/home/user/project/crates/foo/src/lib.rs"),
        "crates/foo/src/lib.rs"
    );
}

#[test]
fn relative_path_strips_src_prefix() {
    assert_eq!(relative_path("/home/user/project/src/main.rs"), "src/main.rs");
}

#[test]
fn relative_path_no_anchor() {
    assert_eq!(relative_path("lib.rs"), "lib.rs");
}

// ── extract_crate_name ───────────────────────────────────────────────

#[test]
fn extract_crate_name_from_path() {
    assert_eq!(extract_crate_name("crates/rude-core/src/lib.rs"), "rude-core");
}

#[test]
fn extract_crate_name_no_crates() {
    assert_eq!(extract_crate_name("src/main.rs"), "(root)");
}

// ── build_path_aliases + apply_alias ─────────────────────────────────

#[test]
fn path_aliases_basic() {
    let paths = &[
        "crates/foo/src/a.rs",
        "crates/foo/src/b.rs",
        "crates/bar/src/c.rs",
    ];
    let (alias_map, legend) = build_path_aliases(paths);
    // Should have at least 2 aliases
    assert!(legend.len() >= 2, "legend: {legend:?}");
    // apply_alias should shorten the path
    let short = apply_alias("crates/foo/src/a.rs", &alias_map);
    assert!(
        short.starts_with('[') && short.contains("a.rs"),
        "should start with alias: {short}"
    );
}

#[test]
fn apply_alias_no_match() {
    let alias_map = std::collections::BTreeMap::new();
    assert_eq!(apply_alias("src/main.rs", &alias_map), "src/main.rs");
}

#[test]
fn apply_alias_no_slash() {
    let alias_map = std::collections::BTreeMap::new();
    assert_eq!(apply_alias("lib.rs", &alias_map), "lib.rs");
}

// ── lines_str ────────────────────────────────────────────────────────

#[test]
fn lines_str_with_lines() {
    let c = ParsedChunk {
        kind: "function".to_owned(),
        name: "test".to_owned(),
        file: "src/lib.rs".to_owned(),
        lines: Some((10, 20)),
        signature: None,
        calls: vec![],
        call_lines: vec![],
        types: vec![],
        imports: vec![],
        string_args: vec![],
        param_flows: vec![],
        param_types: vec![],
        field_types: vec![],
        local_types: vec![],
        let_call_bindings: vec![],
        field_accesses: vec![],
        return_type: None,
        enum_variants: vec![],
        is_test: false,
    };
    assert_eq!(lines_str(&c), "10-20");
}

#[test]
fn lines_str_no_lines() {
    let c = ParsedChunk {
        kind: "function".to_owned(),
        name: "test".to_owned(),
        file: "src/lib.rs".to_owned(),
        lines: None,
        signature: None,
        calls: vec![],
        call_lines: vec![],
        types: vec![],
        imports: vec![],
        string_args: vec![],
        param_flows: vec![],
        param_types: vec![],
        field_types: vec![],
        local_types: vec![],
        let_call_bindings: vec![],
        field_accesses: vec![],
        return_type: None,
        enum_variants: vec![],
        is_test: false,
    };
    assert_eq!(lines_str(&c), "");
}
