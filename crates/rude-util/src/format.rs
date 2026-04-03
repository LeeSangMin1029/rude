use std::collections::{BTreeMap, BTreeSet};

pub fn format_lines_opt(lines: Option<(usize, usize)>) -> String {
    lines.map_or(String::new(), |(s, e)| format!(":{s}-{e}"))
}

pub fn extract_crate_name(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    if let Some(src_pos) = normalized.find("/src/") {
        let before_src = &normalized[..src_pos];
        if let Some(slash) = before_src.rfind('/') {
            return before_src[slash + 1..].to_owned();
        }
        return before_src.to_owned();
    }
    // path starts with src/ — look for Cargo.toml in CWD
    if normalized.starts_with("src/") {
        if let Ok(content) = std::fs::read_to_string("Cargo.toml") {
            let mut in_pkg = false;
            for line in content.lines() {
                let t = line.trim();
                if t.starts_with('[') { in_pkg = t == "[package]"; continue; }
                if in_pkg && t.starts_with("name") {
                    if let Some(name) = t.split('"').nth(1) {
                        return name.replace('-', "_");
                    }
                }
            }
        }
    }
    "(root)".to_owned()
}

pub fn build_path_aliases(paths: &[&str]) -> (BTreeMap<String, String>, Vec<(String, String)>) {
    let mut dirs: BTreeSet<&str> = BTreeSet::new();
    for p in paths {
        if let Some(i) = p.rfind('/') { dirs.insert(&p[..=i]); }
    }
    let mut crate_dirs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for &dir in &dirs {
        // find /src/ boundary — everything up to and including /src/ is the crate root
        if let Some(src_pos) = dir.find("/src/") {
            let root_end = src_pos + 5; // include "/src/"
            crate_dirs.entry(dir[..root_end].to_owned()).or_default().insert(dir[root_end..].to_owned());
        }
    }
    let mut alias_map: BTreeMap<String, String> = BTreeMap::new();
    let mut legend: Vec<(String, String)> = Vec::new();
    let mut label = b'A';
    for (root, subdirs) in &crate_dirs {
        if label > b'Z' { break; }
        let letter = label as char;
        let root_alias = format!("[{letter}]");
        alias_map.insert(root.clone(), root_alias.clone());
        legend.push((root_alias, root.clone()));
        let mut sub_num = 1u32;
        for subdir in subdirs {
            if subdir.is_empty() { continue; }
            let sub_alias = format!("[{letter}{sub_num}]");
            alias_map.insert(format!("{root}{subdir}"), sub_alias.clone());
            legend.push((sub_alias, format!("  {subdir}")));
            sub_num += 1;
        }
        label += 1;
    }
    (alias_map, legend)
}

pub fn shorten_symbol_name(name: &str) -> &str {
    // "<module::Type as module::Trait>::method" → "Type::method"
    if let Some(inner) = name.strip_prefix('<') {
        if let Some(as_pos) = inner.find(" as ") {
            let type_part = inner[..as_pos].rsplit("::").next().unwrap_or(&inner[..as_pos]);
            if let Some(gt_pos) = inner.find(">::") {
                let method = &inner[gt_pos + 3..];
                // return just "Type::method" — we need to return a str reference
                // can't construct new string, so return from after last >::
                let method_start = name.len() - method.len();
                let _ = (type_part, method_start);
            }
        }
    }
    // "module::path::function_name" → "function_name"
    name.rsplit("::").next().unwrap_or(name)
}

pub fn display_symbol_name(name: &str) -> String {
    if let Some(inner) = name.strip_prefix('<') {
        if let Some(as_pos) = inner.find(" as ") {
            let type_part = inner[..as_pos].rsplit("::").next().unwrap_or(&inner[..as_pos]);
            if let Some(gt_pos) = inner.find(">::") {
                let method = &inner[gt_pos + 3..];
                return format!("{type_part}::{method}");
            }
            return type_part.to_string();
        }
        let no_gt = inner.trim_end_matches('>');
        return no_gt.rsplit("::").next().unwrap_or(no_gt).to_string();
    }
    let short = name.rsplit("::").next().unwrap_or(name);
    short.to_string()
}

pub fn apply_alias(path: &str, alias_map: &BTreeMap<String, String>) -> String {
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => return path.to_owned(),
    };
    if let Some(alias) = alias_map.get(dir) {
        format!("{alias}{}", &path[dir.len()..])
    } else {
        path.to_owned()
    }
}
