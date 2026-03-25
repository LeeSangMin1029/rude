use crate::commands::intel::parse::normalize_path;

#[test]
fn normalize_backslashes() {
    assert_eq!(normalize_path(".\\crates\\foo.rs"), "crates/foo.rs");
    assert_eq!(normalize_path("./src/main.rs"), "src/main.rs");
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
