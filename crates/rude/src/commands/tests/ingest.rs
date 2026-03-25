    use super::*;

    fn make_entry(name: &str, calls: &[&str]) -> CodeChunkEntry {
        CodeChunkEntry {
            chunk: rude_intel::parse::ParsedChunk {
                name: name.to_string(),
                kind: "function".to_string(),
                calls: calls.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            source: "test.rs".to_string(),
            mtime: 0,
            lang: "rust",
        }
    }

    #[test]
    fn called_by_index_simple() {
        let entries = vec![
            make_entry("main", &["foo", "bar"]),
            make_entry("foo", &["bar"]),
        ];
        let index = build_callers(&entries);

        let foo_callers = index.get("foo").unwrap();
        assert!(foo_callers.contains(&"main".to_string()));

        let bar_callers = index.get("bar").unwrap();
        assert!(bar_callers.contains(&"main".to_string()));
        assert!(bar_callers.contains(&"foo".to_string()));
    }

    #[test]
    fn called_by_index_qualified_calls() {
        let entries = vec![
            make_entry("process", &["Module::helper", "self.validate"]),
        ];
        let index = build_callers(&entries);
        assert!(index.get("helper").unwrap().contains(&"process".to_string()));
        assert!(index.get("validate").unwrap().contains(&"process".to_string()));
    }

    #[test]
    fn called_by_index_deduplicates() {
        let entries = vec![
            make_entry("a", &["target"]),
            make_entry("a", &["target"]),
        ];
        let index = build_callers(&entries);
        assert_eq!(index.get("target").unwrap().len(), 1);
    }

    #[test]
    fn called_by_index_empty() {
        let entries: Vec<CodeChunkEntry> = vec![];
        let index = build_callers(&entries);
        assert!(index.is_empty());
    }

    #[test]
    fn find_callers_bare_name() {
        let entries = vec![
            make_entry("caller_a", &["target_fn"]),
            make_entry("caller_b", &["target_fn"]),
        ];
        let reverse = build_callers(&entries);
        let result = find_callers(&reverse, "target_fn");
        assert!(result.contains(&"caller_a"));
        assert!(result.contains(&"caller_b"));
    }

    #[test]
    fn find_callers_qualified_name() {
        let entries = vec![
            make_entry("handler", &["MyStruct::new"]),
        ];
        let reverse = build_callers(&entries);
        let result = find_callers(&reverse, "MyStruct::new");
        assert!(result.contains(&"handler"));
    }

    #[test]
    fn find_callers_excludes_self() {
        let entries = vec![
            make_entry("recursive", &["recursive"]),
        ];
        let reverse = build_callers(&entries);
        let result = find_callers(&reverse, "recursive");
        assert!(result.is_empty(), "self-calls should be excluded");
    }

    #[test]
    fn find_callers_no_match() {
        let entries = vec![
            make_entry("a", &["b"]),
        ];
        let reverse = build_callers(&entries);
        let result = find_callers(&reverse, "nonexistent");
        assert!(result.is_empty());
    }

    #[test]
    fn find_callers_sorted() {
        let entries = vec![
            make_entry("z_caller", &["target"]),
            make_entry("a_caller", &["target"]),
            make_entry("m_caller", &["target"]),
        ];
        let reverse = build_callers(&entries);
        let result = find_callers(&reverse, "target");
        assert_eq!(result, vec!["a_caller", "m_caller", "z_caller"]);
    }
