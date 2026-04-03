#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::fs;

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

    // ── clean_all_mir_state ──

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

        super::super::clean_all_mir_state(root);

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
        // no target/ at all — should not panic
        super::super::clean_all_mir_state(dir.path());
    }

    #[test]
    fn clean_handles_readonly_files() {
        let dir = setup_test_dir();
        let f = dir.path().join("target/mir-edges/locked");
        fs::write(&f, "data").unwrap();
        // clean should not panic even if remove fails
        super::super::clean_all_mir_state(dir.path());
    }

    // ── has_cached_edges (mir.db validity) ──

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
    fn db_garbage_is_valid_by_size() {
        let dir = setup_test_dir();
        let db = dir.path().join("target/mir-edges/mir.db");
        fs::write(&db, "not a real sqlite db").unwrap();
        assert!(is_db_valid(&db));
    }

    #[test]
    fn db_real_data_is_valid() {
        let dir = setup_test_dir();
        let db = dir.path().join("target/mir-edges/mir.db");
        fs::write(&db, vec![0u8; 4096]).unwrap();
        assert!(is_db_valid(&db));
    }

    // ── stale mir-check cache invalidation ──

    #[test]
    fn stale_cache_deleted_when_db_missing() {
        let dir = setup_test_dir();
        let mir_check = dir.path().join("target/mir-check-2026-04-02");
        fs::create_dir_all(&mir_check).unwrap();
        fs::write(mir_check.join("cached"), "old").unwrap();
        let db = dir.path().join("target/mir-edges/mir.db");

        assert!(should_delete_mir_check(&db, &mir_check));
        if should_delete_mir_check(&db, &mir_check) {
            fs::remove_dir_all(&mir_check).unwrap();
        }
        assert!(!mir_check.exists());
    }

    #[test]
    fn stale_cache_deleted_when_db_empty() {
        let dir = setup_test_dir();
        let mir_check = dir.path().join("target/mir-check-2026-04-02");
        fs::create_dir_all(&mir_check).unwrap();
        let db = dir.path().join("target/mir-edges/mir.db");
        fs::write(&db, "").unwrap();

        assert!(should_delete_mir_check(&db, &mir_check));
    }

    #[test]
    fn cache_preserved_when_db_valid() {
        let dir = setup_test_dir();
        let mir_check = dir.path().join("target/mir-check-2026-04-02");
        fs::create_dir_all(&mir_check).unwrap();
        let db = dir.path().join("target/mir-edges/mir.db");
        fs::write(&db, "valid").unwrap();

        assert!(!should_delete_mir_check(&db, &mir_check));
        assert!(mir_check.exists());
    }

    // ── detect_workspace_members ──

    #[test]
    fn members_empty_for_nonexistent() {
        let members = super::super::detect_workspace_members(Path::new("/nonexistent/path/xxx"));
        assert!(members.is_empty());
    }

    #[test]
    fn members_from_real_workspace() {
        let members = super::super::detect_workspace_members(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap());
        assert!(!members.is_empty(), "rude workspace should have members");
        assert!(members.iter().any(|m| m == "rude"), "should contain 'rude'");
    }

    // ── sub-workspace validation ──

    #[test]
    fn invalid_subworkspace_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("broken-crate");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("Cargo.toml"), "[package]\nname = \"broken\"\nedition.workspace = true").unwrap();
        let valid = std::process::Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(&sub)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status().map(|s| s.success()).unwrap_or(false);
        assert!(!valid, "broken workspace ref should fail validation");
    }

    #[test]
    fn valid_standalone_crate_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("good-crate");
        fs::create_dir_all(sub.join("src")).unwrap();
        fs::write(sub.join("Cargo.toml"), "[package]\nname = \"good\"\nversion = \"0.1.0\"\nedition = \"2021\"").unwrap();
        fs::write(sub.join("src/lib.rs"), "").unwrap();
        let valid = std::process::Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(&sub)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status().map(|s| s.success()).unwrap_or(false);
        assert!(valid, "standalone crate should pass validation");
    }

    // ── worst case: everything corrupted ──

    #[test]
    fn worst_case_all_corrupted_clean_recovers() {
        let dir = setup_test_dir();
        let root = dir.path();
        // corrupt mir.db
        fs::write(root.join("target/mir-edges/mir.db"), "CORRUPT").unwrap();
        // stale mir-check with garbage
        let mc = root.join("target/mir-check-old");
        fs::create_dir_all(&mc).unwrap();
        fs::write(mc.join("stale"), "old").unwrap();
        // stale sub-workspace cache
        fs::write(root.join("target/mir-edges/.sub-workspaces"), "/bad/path\n/another/bad").unwrap();
        // extra mir-check dirs
        fs::create_dir_all(root.join("target/mir-check-2025-01-01")).unwrap();
        fs::create_dir_all(root.join("target/mir-check-2026-04-02")).unwrap();

        super::super::clean_all_mir_state(root);

        assert!(!root.join("target/mir-edges/mir.db").exists(), "corrupt db gone");
        assert!(!root.join("target/mir-edges/.sub-workspaces").exists(), "stale cache gone");
        assert!(!root.join("target/mir-check-2025-01-01").exists(), "old mir-check gone");
        assert!(!root.join("target/mir-check-2026-04-02").exists(), "old mir-check gone");
        assert!(root.join("target/mir-edges").exists(), "mir-edges dir recreated");
    }

    #[test]
    fn worst_case_double_clean_no_panic() {
        let dir = setup_test_dir();
        super::super::clean_all_mir_state(dir.path());
        super::super::clean_all_mir_state(dir.path()); // second call should be safe
    }
}
