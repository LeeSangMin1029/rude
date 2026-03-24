use crate::commands::intel::parse::ParsedChunk;

pub fn chunk(name: &str, file: &str, calls: &[&str]) -> ParsedChunk {
    ParsedChunk {
        kind: "function".to_owned(),
        name: name.to_owned(),
        file: file.to_owned(),
        lines: Some((1, 10)),
        signature: Some(format!("fn {name}()")),
        calls: calls.iter().map(|s| s.to_string()).collect(),
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

pub fn test_chunk(name: &str, file: &str, calls: &[&str]) -> ParsedChunk {
    ParsedChunk {
        kind: "function".to_owned(),
        name: name.to_owned(),
        file: format!("src/tests/{file}"),
        lines: Some((1, 10)),
        signature: Some(format!("fn {name}()")),
        calls: calls.iter().map(|s| s.to_string()).collect(),
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
