use rude_util::{
    apply_alias, build_path_aliases, extract_crate_name, format_lines_opt, relative_path,
};

#[test]
fn format_lines_opt_some() {
    assert_eq!(format_lines_opt(Some((5, 15))), ":5-15");
}

#[test]
fn format_lines_opt_none() {
    assert_eq!(format_lines_opt(None), "");
}

#[test]
fn relative_path_strips_crates_prefix() {
    assert_eq!(relative_path("/home/user/project/crates/foo/src/lib.rs"), "crates/foo/src/lib.rs");
}

#[test]
fn relative_path_strips_src_prefix() {
    assert_eq!(relative_path("/home/user/project/src/main.rs"), "src/main.rs");
}

#[test]
fn relative_path_no_anchor() {
    assert_eq!(relative_path("lib.rs"), "lib.rs");
}

#[test]
fn extract_crate_name_from_path() {
    assert_eq!(extract_crate_name("crates/rude-core/src/lib.rs"), "rude-core");
}

#[test]
fn extract_crate_name_no_crates() {
    assert_eq!(extract_crate_name("src/main.rs"), "(root)");
}

#[test]
fn path_aliases_basic() {
    let paths = &["crates/foo/src/a.rs", "crates/foo/src/b.rs", "crates/bar/src/c.rs"];
    let (alias_map, legend) = build_path_aliases(paths);
    assert!(legend.len() >= 2);
    let short = apply_alias("crates/foo/src/a.rs", &alias_map);
    assert!(short.starts_with('[') && short.contains("a.rs"));
}

#[test]
fn apply_alias_no_match() {
    assert_eq!(apply_alias("src/main.rs", &std::collections::BTreeMap::new()), "src/main.rs");
}

#[test]
fn apply_alias_no_slash() {
    assert_eq!(apply_alias("lib.rs", &std::collections::BTreeMap::new()), "lib.rs");
}
