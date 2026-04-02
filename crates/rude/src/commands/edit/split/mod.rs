mod single;
mod module;

pub use single::split;
pub use module::{split_module, split_module_auto};

use std::path::{Path, PathBuf};
use anyhow::Result;
use crate::commands::edit::file::locked_edit;

fn sort_by_kind(ranges: &mut [(usize, usize, &str, &str)]) {
    let kind_order = |k: &str| match k { "struct" | "enum" | "trait" => 0, "impl" => 1, _ => 2 };
    ranges.sort_by(|a, b| kind_order(a.3).cmp(&kind_order(b.3)).then(a.0.cmp(&b.0)));
}

fn build_file_content(inner_attrs: &[String], use_lines: &[String], ranges: &[(usize, usize, &str, &str)], source_lines: &[&str]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !inner_attrs.is_empty() { parts.extend(inner_attrs.iter().cloned()); parts.push(String::new()); }
    if !use_lines.is_empty() { parts.extend(use_lines.iter().cloned()); parts.push(String::new()); }
    for (i, &(start, end, _, _)) in ranges.iter().enumerate() {
        if i > 0 { parts.push(String::new()); }
        parts.extend(source_lines[start..=end].iter().map(|l| l.to_string()));
    }
    parts.join("\n") + "\n"
}

fn is_header_line(t: &str) -> bool {
    t.is_empty() || t.starts_with("use ") || t.starts_with("pub use ")
        || t.starts_with("//") || t.starts_with("#[") || t.starts_with("#![")
        || t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("extern ")
}

fn filter_header(source: &str, body: &str) -> (Vec<String>, Vec<String>) {
    let mut inner_attrs = Vec::new();
    let mut use_lines = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if !is_header_line(trimmed) { break; }
        if trimmed.starts_with("#![") {
            inner_attrs.push(line.to_string());
        } else if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") || trimmed.starts_with("pub(crate) use ") {
            if trimmed.contains("::*") {
                use_lines.push(line.to_string());
            } else {
                let idents = crate::commands::edit::imports::extract_use_idents(trimmed);
                if idents.iter().any(|id| crate::commands::edit::imports::ident_used_in(body, id)) {
                    use_lines.push(line.to_string());
                }
            }
        }
    }
    (inner_attrs, use_lines)
}

fn find_mod_file(dir: &Path) -> Option<PathBuf> {
    [dir.join("lib.rs"), dir.join("mod.rs"), dir.join("main.rs")]
        .into_iter().find(|p| p.exists())
}

fn insert_line_after(path: &Path, matcher: impl Fn(&str) -> bool, line: &str, skip_if: Option<&str>) -> Result<()> {
    locked_edit(path, |content| {
        if let Some(check) = skip_if {
            if content.lines().any(|l| l.trim() == check) { return Ok(content.to_string()); }
        }
        let lines: Vec<&str> = content.lines().collect();
        let pos = lines.iter().rposition(|l| !l.starts_with(' ') && !l.starts_with('\t') && matcher(l.trim())).map(|i| i + 1).unwrap_or(0);
        let mut result: Vec<String> = Vec::with_capacity(lines.len() + 2);
        for (i, &l) in lines.iter().enumerate() {
            result.push(l.to_string());
            if i + 1 == pos { result.push(line.to_string()); }
        }
        if pos == 0 { result.insert(0, line.to_string()); if !lines.is_empty() { result.insert(1, String::new()); } }
        let trailing = content.ends_with('\n');
        Ok(if trailing { result.join("\n") + "\n" } else { result.join("\n") })
    })
}

fn insert_reexport(path: &Path, reexport_line: &str) -> Result<()> {
    insert_line_after(path, |t| t.starts_with("use ") || t.starts_with("pub use "), reexport_line, None)
}

fn insert_mod_decl(path: &Path, mod_decl: &str, module_name: &str) -> Result<()> {
    let check = format!("mod {module_name};");
    insert_line_after(path,
        |t| (t.starts_with("mod ") || t.starts_with("pub mod ") || t.starts_with("pub(crate) mod ")) && t.ends_with(';'),
        mod_decl, Some(&check))
}
