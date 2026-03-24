//! Unit tests for `stats` module — per-crate statistics.

use crate::parse::ParsedChunk;
use crate::stats::build_stats;

fn make_chunk(kind: &str, name: &str, file: &str) -> ParsedChunk {
    ParsedChunk {
        kind: kind.to_owned(),
        name: name.to_owned(),
        file: file.to_owned(),
        lines: Some((1, 10)),
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
    }
}

#[test]
fn stats_counts_functions() {
    let chunks = vec![
        make_chunk("function", "foo", "crates/core/src/lib.rs"),
        make_chunk("function", "bar", "crates/core/src/util.rs"),
        make_chunk("function", "test_baz", "crates/core/src/tests/baz.rs"),
    ];
    let stats = build_stats(&chunks);
    let core = stats.get("core").expect("should have 'core' entry");
    assert_eq!(core[0], 2, "prod_fn should be 2");
    assert_eq!(core[1], 1, "test_fn should be 1");
}

#[test]
fn stats_counts_structs_enums() {
    let chunks = vec![
        make_chunk("struct", "Config", "crates/types/src/lib.rs"),
        make_chunk("enum", "Status", "crates/types/src/lib.rs"),
        make_chunk("struct", "Options", "crates/types/src/lib.rs"),
    ];
    let stats = build_stats(&chunks);
    let types = stats.get("types").expect("should have 'types' entry");
    assert_eq!(types[2], 2, "struct count should be 2");
    assert_eq!(types[3], 1, "enum count should be 1");
}

#[test]
fn stats_empty_chunks() {
    let stats = build_stats(&[]);
    assert!(stats.is_empty());
}

