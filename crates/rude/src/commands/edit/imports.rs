use std::path::Path;
use anyhow::Result;
use super::file::locked_edit;

pub(crate) fn cleanup_unused_imports(path: &Path) -> Result<()> {
    locked_edit(path, |content| {
        let lines: Vec<&str> = content.lines().collect();
        let trailing = content.ends_with('\n');
        let mut keep = vec![true; lines.len()];
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if !is_use_line(trimmed) { continue; }
            let idents = extract_use_idents(trimmed);
            if idents.is_empty() { continue; }
            let rest: String = lines.iter().enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, l)| *l)
                .collect::<Vec<_>>()
                .join("\n");
            if idents.iter().all(|id| !ident_used_in(&rest, id)) {
                keep[i] = false;
            }
        }
        let mut result: Vec<&str> = Vec::new();
        let mut prev_removed = false;
        for (i, &line) in lines.iter().enumerate() {
            if !keep[i] {
                prev_removed = true;
                continue;
            }
            if prev_removed && line.trim().is_empty() && result.last().map_or(false, |l: &&str| l.trim().is_empty()) {
                continue;
            }
            prev_removed = false;
            result.push(line);
        }
        let mut out = result.join("\n");
        if trailing { out.push('\n'); }
        Ok(out)
    })
}

pub(crate) fn ensure_import(path: &Path, import: &str) -> Result<()> {
    let vis_prefix = if import.starts_with("pub use ") || import.starts_with("pub(crate) use ") {
        import.split("use ").next().unwrap_or("")
    } else { "" };
    let use_part = import.trim_start_matches("pub(crate) ").trim_start_matches("pub ");
    let use_part = use_part.trim_start_matches("use ").trim_end_matches(';');
    let (crate_path, item) = match use_part.rsplit_once("::") {
        Some((p, i)) => (p, i.trim_matches(|c| c == '{' || c == '}' || c == ' ')),
        None => return Ok(()),
    };
    let new_items: Vec<&str> = item.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    locked_edit(path, |content| {
        let lines: Vec<&str> = content.lines().collect();
        let trailing = content.ends_with('\n');
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if !trimmed.contains(&format!("{crate_path}::")) { continue; }
            if !is_use_line(trimmed) { continue; }
            let line_vis = if trimmed.starts_with("pub(crate) use ") { "pub(crate) " }
                else if trimmed.starts_with("pub use ") { "pub " }
                else { "" };
            let after_use = trimmed
                .trim_start_matches("pub(crate) ").trim_start_matches("pub ")
                .trim_start_matches("use ").trim_end_matches(';');
            let (existing_path, existing_items) = match after_use.rsplit_once("::") {
                Some((p, i)) => (p, i),
                None => continue,
            };
            if existing_path != crate_path { continue; }
            let mut items: Vec<String> = if existing_items.starts_with('{') {
                existing_items.trim_matches(|c| c == '{' || c == '}')
                    .split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect()
            } else {
                vec![existing_items.trim().to_owned()]
            };
            let mut changed = false;
            for &new in &new_items {
                if !items.iter().any(|e| e == new) {
                    items.push(new.to_owned());
                    changed = true;
                }
            }
            if !changed { return Ok(content.to_string()); }
            items.sort();
            let merged_vis = if !vis_prefix.is_empty() { vis_prefix } else { line_vis };
            let new_line = if items.len() == 1 {
                format!("{merged_vis}use {crate_path}::{};", items[0])
            } else {
                format!("{merged_vis}use {crate_path}::{{{}}};", items.join(", "))
            };
            let mut result: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            result[i] = new_line;
            let mut out = result.join("\n");
            if trailing { out.push('\n'); }
            return Ok(out);
        }
        let new_line = if new_items.len() == 1 {
            format!("{vis_prefix}use {crate_path}::{};", new_items[0])
        } else {
            format!("{vis_prefix}use {crate_path}::{{{}}};", new_items.join(", "))
        };
        let insert_pos = lines.iter().rposition(|l| {
            let t = l.trim();
            t.starts_with("use ") || t.starts_with("pub use ") || t.starts_with("pub(crate) use ")
        }).map(|i| i + 1).unwrap_or(0);
        let mut result: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        result.insert(insert_pos, new_line);
        let mut out = result.join("\n");
        if trailing { out.push('\n'); }
        Ok(out)
    })
}

fn is_use_line(trimmed: &str) -> bool {
    (trimmed.starts_with("use ") || trimmed.starts_with("pub use ") || trimmed.starts_with("pub(crate) use "))
        && trimmed.ends_with(';')
}

pub(crate) fn extract_use_idents(use_line: &str) -> Vec<String> {
    let after_use = use_line
        .trim_start_matches("pub(crate) ").trim_start_matches("pub ")
        .trim_start_matches("use ").trim_end_matches(';');
    let (_, items_part) = match after_use.rsplit_once("::") {
        Some(pair) => pair,
        None => return vec![after_use.to_owned()],
    };
    if items_part.starts_with('{') {
        items_part.trim_matches(|c| c == '{' || c == '}')
            .split(',')
            .map(|s| {
                let s = s.trim();
                if let Some((_, alias)) = s.split_once(" as ") {
                    alias.trim().to_owned()
                } else {
                    s.to_owned()
                }
            })
            .filter(|s| !s.is_empty() && s != "*")
            .collect()
    } else if items_part == "*" {
        vec![]
    } else if let Some((_, alias)) = items_part.split_once(" as ") {
        vec![alias.trim().to_owned()]
    } else {
        vec![items_part.trim().to_owned()]
    }
}

pub(crate) fn ident_used_in(code: &str, ident: &str) -> bool {
    for (i, _) in code.match_indices(ident) {
        let before = if i > 0 { code.as_bytes()[i - 1] } else { b' ' };
        let after_idx = i + ident.len();
        let after = if after_idx < code.len() { code.as_bytes()[after_idx] } else { b' ' };
        if !is_ident_char(before) && !is_ident_char(after) {
            let line_start = code[..i].rfind('\n').map(|p| p + 1).unwrap_or(0);
            let before_on_line = code[line_start..i].trim();
            if before_on_line.starts_with("use ") || before_on_line.starts_with("pub use ")
                || before_on_line.starts_with("pub(crate) use ") {
                continue;
            }
            if before_on_line.starts_with("//") { continue; }
            return true;
        }
    }
    false
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
