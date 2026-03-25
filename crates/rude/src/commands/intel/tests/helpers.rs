use crate::commands::intel::parse::ParsedChunk;

pub fn chunk(name: &str, file: &str, calls: &[&str]) -> ParsedChunk {
    ParsedChunk {
        kind: "function".to_owned(),
        name: name.to_owned(),
        file: file.to_owned(),
        lines: Some((1, 10)),
        signature: Some(format!("fn {name}()")),
        calls: calls.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
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
        ..Default::default()
    }
}
