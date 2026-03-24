use crate::commands::intel::parse::*;

#[test]
fn parse_simple_function() {
    let text = "[function] topological_sort\n\
                 File: .\\crates\\swarm-cli\\src\\engine.rs:47-54\n\
                 Signature: fn topological_sort(dag: &Dag) -> Option<Vec<String>>\n\
                 Calls: toposort, sort_by";
    let chunk = parse_chunk(text).unwrap();
    assert_eq!(chunk.kind, "function");
    assert_eq!(chunk.name, "topological_sort");
    assert_eq!(chunk.file, "crates/swarm-cli/src/engine.rs");
    assert_eq!(chunk.lines, Some((47, 54)));
    assert_eq!(chunk.calls, vec!["toposort", "sort_by"]);
}

#[test]
fn parse_pub_method() {
    let text = "[function] pub CodeChunk::parse\n\
                 File: crates/rude-intel/src/parse.rs:51-120\n\
                 Doc comment here\n\
                 Signature: pub fn parse(text: &str) -> Option<CodeChunk>\n\
                 Types: CodeChunk, String\n\
                 Calls: String::new, lines.next";
    let chunk = parse_chunk(text).unwrap();
    assert_eq!(chunk.name, "CodeChunk::parse");
    assert_eq!(chunk.types, vec!["CodeChunk", "String"]);
    assert_eq!(chunk.calls, vec!["String::new", "lines.next"]);
}

#[test]
fn parse_non_code_returns_none() {
    let text = "Just a regular markdown paragraph.";
    assert!(parse_chunk(text).is_none());
}

#[test]
fn normalize_backslashes() {
    assert_eq!(normalize_path(".\\crates\\foo.rs"), "crates/foo.rs");
    assert_eq!(normalize_path("./src/main.rs"), "src/main.rs");
}

// ── Edge case tests ─────────────────────────────────────────────────

#[test]
fn parse_empty_string_returns_none() {
    assert!(parse_chunk("").is_none());
}

#[test]
fn parse_no_bracket_returns_none() {
    assert!(parse_chunk("function topological_sort").is_none());
}

#[test]
fn parse_bracket_but_no_file_returns_none() {
    let text = "[function] my_func\nSignature: fn my_func()";
    assert!(
        parse_chunk(text).is_none(),
        "chunk without File: line should return None"
    );
}

#[test]
fn parse_unclosed_bracket_returns_none() {
    let text = "[function my_func\nFile: src/main.rs:1-10";
    assert!(parse_chunk(text).is_none(), "unclosed bracket should return None");
}

#[test]
fn parse_file_without_line_range() {
    let text = "[struct] MyStruct\nFile: src/types.rs";
    let chunk = parse_chunk(text).unwrap();
    assert_eq!(chunk.kind, "struct");
    assert_eq!(chunk.name, "MyStruct");
    assert_eq!(chunk.file, "src/types.rs");
    assert_eq!(chunk.lines, None, "no line range should result in None");
}

#[test]
fn parse_empty_calls_and_types() {
    let text = "[function] foo\nFile: src/lib.rs:1-5";
    let chunk = parse_chunk(text).unwrap();
    assert!(chunk.calls.is_empty());
    assert!(chunk.types.is_empty());
    assert!(chunk.signature.is_none());
}

#[test]
fn parse_kind_with_spaces_after_bracket() {
    let text = "[enum] pub Status\nFile: src/status.rs:10-20";
    let chunk = parse_chunk(text).unwrap();
    assert_eq!(chunk.kind, "enum");
    assert_eq!(chunk.name, "Status", "name should be last token after visibility");
}

#[test]
fn parse_name_with_double_colon() {
    let text = "[function] pub Outer::Inner::method\nFile: src/nested.rs:5-15\nSignature: fn method(&self)";
    let chunk = parse_chunk(text).unwrap();
    assert_eq!(chunk.name, "Outer::Inner::method");
}

#[test]
fn parse_forward_slash_path_preserved() {
    let text = "[function] run\nFile: crates/rude-cli/src/main.rs:1-50";
    let chunk = parse_chunk(text).unwrap();
    assert_eq!(chunk.file, "crates/rude-cli/src/main.rs");
    assert_eq!(chunk.lines, Some((1, 50)));
}

#[test]
fn normalize_path_already_unix() {
    assert_eq!(normalize_path("src/main.rs"), "src/main.rs");
}

#[test]
fn normalize_path_no_prefix() {
    assert_eq!(normalize_path("crates/foo/bar.rs"), "crates/foo/bar.rs");
}

#[test]
fn normalize_path_deep_windows() {
    assert_eq!(
        normalize_path(".\\crates\\rude-cli\\src\\main.rs"),
        "crates/rude-cli/src/main.rs"
    );
}

#[test]
fn parse_multiple_calls() {
    let text = "[function] process\nFile: src/proc.rs:1-10\nCalls: foo, bar, baz::quux, Vec::new";
    let chunk = parse_chunk(text).unwrap();
    assert_eq!(chunk.calls, vec!["foo", "bar", "baz::quux", "Vec::new"]);
}

#[test]
fn parse_multiple_types() {
    let text = "[function] transform\nFile: src/xform.rs:1-5\nTypes: HashMap, Vec, Option, Result";
    let chunk = parse_chunk(text).unwrap();
    assert_eq!(chunk.types, vec!["HashMap", "Vec", "Option", "Result"]);
}

#[test]
fn parse_only_first_line_matters_for_kind() {
    // Extra lines with random content should not break parsing
    let text = "[trait] MyTrait\nFile: src/traits.rs:1-20\nSome random text\nAnother random line";
    let chunk = parse_chunk(text).unwrap();
    assert_eq!(chunk.kind, "trait");
    assert_eq!(chunk.name, "MyTrait");
}

#[test]
fn parse_windows_path_with_colon_in_drive() {
    // Edge case: Windows absolute path like C:\foo\bar.rs:1-10
    // The parser uses rfind(':') so it should handle this
    let text = "[function] main\nFile: C:\\Users\\dev\\src\\main.rs:1-50";
    let chunk = parse_chunk(text).unwrap();
    assert_eq!(chunk.lines, Some((1, 50)));
    // File should have backslashes normalized
    assert!(chunk.file.contains("main.rs"));
}
