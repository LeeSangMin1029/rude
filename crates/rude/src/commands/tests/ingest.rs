    use super::*;

    fn make_entry(name: &str, calls: &[&str]) -> CodeChunkEntry {
        CodeChunkEntry {
            chunk: chunk_code::CodeChunk {
                name: name.to_string(),
                kind: chunk_code::CodeNodeKind::Function,
                text: String::new(),
                calls: calls.iter().map(|s| s.to_string()).collect(),
                call_lines: vec![],
                type_refs: vec![],
                signature: None,
                doc_comment: None,
                visibility: String::new(),
                chunk_index: 0,
                start_line: 0,
                end_line: 0,
                start_byte: 0,
                end_byte: 0,
                imports: vec![],
                param_types: vec![],
                return_type: None,
                ast_hash: 0,
                body_hash: 0,
                sub_blocks: vec![],
                string_args: vec![],
                param_flows: vec![],
                field_types: vec![],
                local_types: vec![],
                let_call_bindings: vec![],
                field_accesses: vec![],
                enum_variants: vec![],
                is_test: false,
            },
            source: "test.rs".to_string(),
            file_path_str: "test.rs".to_string(),
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
