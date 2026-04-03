use std::fs;
use std::path::Path;

fn setup_test_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("target/mir-edges")).unwrap();
    dir
}

fn is_db_valid(db_path: &Path) -> bool {
    db_path.exists() && fs::metadata(db_path).map_or(false, |m| m.len() > 0)
}

fn should_delete_mir_check(db_path: &Path, mir_check: &Path) -> bool {
    !is_db_valid(db_path) && mir_check.exists()
}

#[test]
fn clean_removes_mir_edges_contents_and_mir_check_dirs() {
    let dir = setup_test_dir();
    let root = dir.path();
    fs::create_dir_all(root.join("target/mir-check-2026-04-01")).unwrap();
    fs::create_dir_all(root.join("target/mir-check-2026-04-02")).unwrap();
    fs::write(root.join("target/mir-edges/mir.db"), "data").unwrap();
    fs::write(root.join("target/mir-edges/extra"), "x").unwrap();
    fs::create_dir_all(root.join("target/debug")).unwrap();
    fs::write(root.join("target/debug/keep"), "y").unwrap();

    rude_cli::commands::add::run::mir::clean_all_mir_state(root);

    assert!(root.join("target/mir-edges").exists());
    assert!(!root.join("target/mir-edges/mir.db").exists());
    assert!(!root.join("target/mir-edges/extra").exists());
    assert!(!root.join("target/mir-check-2026-04-01").exists());
    assert!(!root.join("target/mir-check-2026-04-02").exists());
    assert!(root.join("target/debug/keep").exists());
}

#[test]
fn clean_handles_missing_target() {
    let dir = tempfile::tempdir().unwrap();
    rude_cli::commands::add::run::mir::clean_all_mir_state(dir.path());
}

#[test]
fn clean_handles_double_call() {
    let dir = setup_test_dir();
    rude_cli::commands::add::run::mir::clean_all_mir_state(dir.path());
    rude_cli::commands::add::run::mir::clean_all_mir_state(dir.path());
}

#[test]
fn db_missing_is_invalid() {
    let dir = setup_test_dir();
    assert!(!is_db_valid(&dir.path().join("target/mir-edges/mir.db")));
}

#[test]
fn db_empty_is_invalid() {
    let dir = setup_test_dir();
    let db = dir.path().join("target/mir-edges/mir.db");
    fs::write(&db, "").unwrap();
    assert!(!is_db_valid(&db));
}

#[test]
fn db_with_data_is_valid() {
    let dir = setup_test_dir();
    let db = dir.path().join("target/mir-edges/mir.db");
    fs::write(&db, "valid data").unwrap();
    assert!(is_db_valid(&db));
}

#[test]
fn stale_cache_deleted_when_db_missing() {
    let dir = setup_test_dir();
    let mc = dir.path().join("target/mir-check-2026-04-02");
    fs::create_dir_all(&mc).unwrap();
    assert!(should_delete_mir_check(&dir.path().join("target/mir-edges/mir.db"), &mc));
}

#[test]
fn stale_cache_deleted_when_db_empty() {
    let dir = setup_test_dir();
    let mc = dir.path().join("target/mir-check-2026-04-02");
    fs::create_dir_all(&mc).unwrap();
    let db = dir.path().join("target/mir-edges/mir.db");
    fs::write(&db, "").unwrap();
    assert!(should_delete_mir_check(&db, &mc));
}

#[test]
fn cache_preserved_when_db_valid() {
    let dir = setup_test_dir();
    let mc = dir.path().join("target/mir-check-2026-04-02");
    fs::create_dir_all(&mc).unwrap();
    let db = dir.path().join("target/mir-edges/mir.db");
    fs::write(&db, "valid").unwrap();
    assert!(!should_delete_mir_check(&db, &mc));
}

#[test]
fn members_empty_for_nonexistent() {
    let m = rude_cli::commands::add::run::mir::detect_workspace_members(Path::new("/nonexistent/xxx"));
    assert!(m.is_empty());
}

#[test]
fn members_from_real_workspace() {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    let m = rude_cli::commands::add::run::mir::detect_workspace_members(ws);
    assert!(!m.is_empty());
    assert!(m.iter().any(|x| x == "rude"));
}

#[test]
fn invalid_subworkspace_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("broken");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("Cargo.toml"), "[package]\nname=\"x\"\nedition.workspace=true").unwrap();
    let ok = std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(&sub).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false);
    assert!(!ok);
}

#[test]
fn valid_standalone_crate_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("good");
    fs::create_dir_all(sub.join("src")).unwrap();
    fs::write(sub.join("Cargo.toml"), "[package]\nname=\"good\"\nversion=\"0.1.0\"\nedition=\"2021\"").unwrap();
    fs::write(sub.join("src/lib.rs"), "").unwrap();
    let ok = std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(&sub).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false);
    assert!(ok);
}

#[test]
fn worst_case_all_corrupted_clean_recovers() {
    let dir = setup_test_dir();
    let root = dir.path();
    fs::write(root.join("target/mir-edges/mir.db"), "CORRUPT").unwrap();
    fs::create_dir_all(root.join("target/mir-check-2025-01-01")).unwrap();
    fs::create_dir_all(root.join("target/mir-check-2026-04-02")).unwrap();
    fs::write(root.join("target/mir-edges/.sub-workspaces"), "/bad\n").unwrap();

    rude_cli::commands::add::run::mir::clean_all_mir_state(root);

    assert!(!root.join("target/mir-edges/mir.db").exists());
    assert!(!root.join("target/mir-check-2025-01-01").exists());
    assert!(!root.join("target/mir-check-2026-04-02").exists());
    assert!(root.join("target/mir-edges").exists());
}
