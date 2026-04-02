mod mir;
mod writer;
pub(crate) mod polyglot;

pub(crate) use mir::ingest_mir;
pub(crate) use writer::write_chunks;

use rude_intel::parse::ParsedChunk;

pub(crate) struct CodeChunkEntry {
    pub chunk: ParsedChunk,
}

fn parse_param_types(signature: Option<&str>) -> Vec<(String, String)> {
    signature
        .and_then(|sig| {
            let paren_start = sig.find('(')?;
            let paren_end = sig.rfind(')')?;
            if paren_start >= paren_end {
                return None;
            }
            let params_str = &sig[paren_start + 1..paren_end];
            let pairs: Vec<(String, String)> = params_str
                .split(',')
                .filter_map(|p| {
                    let p = p.trim();
                    if p == "self" || p == "&self" || p == "&mut self" || p.is_empty() {
                        return None;
                    }
                    let (name, ty) = p.split_once(':')?;
                    Some((name.trim().to_owned(), ty.trim().to_owned()))
                })
                .collect();
            Some(pairs)
        })
        .unwrap_or_default()
}

fn parse_return_type(signature: Option<&str>) -> Option<String> {
    signature.and_then(|sig| {
        let after_arrow = sig.split("->").nth(1)?;
        let rt = after_arrow.trim().trim_end_matches('{').trim();
        if rt.is_empty() { None } else { Some(rt.to_owned()) }
    })
}

fn parse_field_types(chunk_lines: &[&str]) -> Vec<(String, String)> {
    if chunk_lines.len() <= 1 {
        return Vec::new();
    }
    chunk_lines[1..]
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim().trim_end_matches(',');
            if trimmed.starts_with("//") || trimmed.is_empty() || trimmed == "}" {
                return None;
            }
            let stripped = trimmed
                .strip_prefix("pub(crate) ")
                .or_else(|| trimmed.strip_prefix("pub(super) "))
                .or_else(|| trimmed.strip_prefix("pub "))
                .unwrap_or(trimmed);
            let (name, ty) = stripped.split_once(':')?;
            Some((name.trim().to_owned(), ty.trim().to_owned()))
        })
        .collect()
}

fn parse_enum_variants(chunk_lines: &[&str]) -> Vec<String> {
    if chunk_lines.len() <= 1 {
        return Vec::new();
    }
    chunk_lines[1..]
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim().trim_end_matches(',');
            if trimmed.starts_with("//")
                || trimmed.is_empty()
                || trimmed == "}"
                || trimmed.starts_with('#')
            {
                return None;
            }
            let name = trimmed
                .split(|c: char| c == '(' || c == '{' || c == ' ')
                .next()?;
            if name.is_empty() { None } else { Some(name.to_owned()) }
        })
        .collect()
}
