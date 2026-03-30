
use std::collections::HashMap;
use crate::data::parse::ParsedChunk;
pub(crate) fn collect_owner_field_types(chunks: &[ParsedChunk]) -> HashMap<String, HashMap<String, String>> {
    let mut result: HashMap<String, HashMap<String, String>> = HashMap::new();
    for c in chunks.iter().filter(|c| c.kind == "struct") {
        let key = owner_leaf(&c.name);
        let entry = result.entry(key).or_default();
        for (fname, ftype) in &c.field_types {
            entry.insert(fname.to_lowercase(), ftype.to_lowercase());
        }
    }
    result
}
pub(crate) fn build_field_access_index(
    chunks: &[ParsedChunk],
    owner_field_types: &HashMap<String, HashMap<String, String>>,
) -> Vec<(String, Vec<u32>)> {
    let mut map: HashMap<String, Vec<u32>> = HashMap::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        if chunk.field_accesses.is_empty() { continue; }
        let mut recv_types: HashMap<String, String> = HashMap::new();

        // self → owning type
        if let Some(owner) = owning_type(&chunk.name) {
            recv_types.insert("self".to_owned(), owner.clone());
            if let Some(fields) = owner_field_types.get(&owner) {
                for (fname, fty) in fields {
                    recv_types.entry(format!("self.{fname}")).or_insert_with(|| {
                        extract_leaf_type(fty).to_owned()
                    });
                }
            }
        }
        // param types
        for (pname, pty) in &chunk.param_types {
            let leaf = extract_leaf_type(&pty.to_lowercase()).to_owned();
            if !leaf.is_empty() && !pname.eq_ignore_ascii_case("self") {
                recv_types.entry(pname.to_lowercase()).or_insert(leaf);
            }
        }
        // local types
        for (vname, vty) in &chunk.local_types {
            let leaf = extract_leaf_type(&vty.to_lowercase()).to_owned();
            if !leaf.is_empty() { recv_types.entry(vname.to_lowercase()).or_insert(leaf); }
        }

        for (recv, field) in &chunk.field_accesses {
            let recv_lower = recv.to_lowercase();
            let ty = recv_types.get(&recv_lower)
                .cloned()
                .unwrap_or_else(|| recv_lower);
            map.entry(format!("{ty}::{}", field.to_lowercase()))
                .or_default().push(idx as u32);
        }
    }
    for list in map.values_mut() { list.sort_unstable(); list.dedup(); }
    let mut result: Vec<_> = map.into_iter().collect();
    result.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    result
}

pub(crate) fn build_trait_impls(
    names: &[&str],
    kinds: &[&str],
    exact: &HashMap<String, u32>,
    short: &HashMap<String, u32>,
) -> Vec<Vec<u32>> {
    let mut trait_impls: Vec<Vec<u32>> = vec![Vec::new(); names.len()];
    for (i, (&name, &kind)) in names.iter().zip(kinds.iter()).enumerate() {
        if kind != "impl" { continue; }
        let lower = name.to_lowercase();
        // Match both "impl Trait for Type" and "<Type as Trait>" naming conventions
        let trait_name = if let Some(pos) = lower.find(" for ") {
            Some(&lower[..pos])
        } else if lower.starts_with('<') {
            // "<type as trait::path>" → extract trait part after " as "
            lower.find(" as ").map(|pos| {
                let after_as = &lower[pos + 4..];
                after_as.strip_suffix('>').unwrap_or(after_as)
            })
        } else {
            None
        };
        if let Some(tname) = trait_name {
            let trait_idx = exact.get(tname).copied()
                .or_else(|| {
                    let leaf = tname.rsplit("::").next().unwrap_or(tname);
                    short.get(leaf).copied()
                });
            if let Some(tidx) = trait_idx {
                trait_impls[tidx as usize].push(i as u32);
            }
        }
    }
    for list in &mut trait_impls { list.sort_unstable(); list.dedup(); }
    trait_impls
}

pub(crate) fn build_fn_trait_impl(
    names: &[&str],
    kinds: &[&str],
) -> Vec<Option<u32>> {
    // Collect impl blocks that are trait impls (name contains " for ")
    let mut trait_impl_set: HashMap<String, u32> = HashMap::new();
    for (i, (&name, &kind)) in names.iter().zip(kinds.iter()).enumerate() {
        if kind == "impl" {
            let lower = name.to_lowercase();
            if lower.contains(" for ") {
                trait_impl_set.insert(lower, i as u32);
            }
        }
    }

    let mut result = vec![None; names.len()];
    for (i, (&name, &kind)) in names.iter().zip(kinds.iter()).enumerate() {
        if kind != "function" { continue; }
        // Extract parent path: "foo::bar::method" → "foo::bar"
        let lower = name.to_lowercase();
        if let Some(pos) = lower.rfind("::") {
            let parent = &lower[..pos];
            if let Some(&impl_idx) = trait_impl_set.get(parent) {
                result[i] = Some(impl_idx);
            }
        }
    }
    result
}
pub fn extract_leaf_type(ty: &str) -> &str {
    let ty = ty.strip_prefix('&').unwrap_or(ty);
    let ty = if ty.starts_with('\'') { ty.find(' ').map_or(ty, |i| &ty[i + 1..]) } else { ty };
    let ty = ty.strip_prefix("mut ").unwrap_or(ty);
    let ty = ty.strip_prefix("dyn ").unwrap_or(ty);
    let ty = ty.strip_prefix("impl ").unwrap_or(ty);
    let outer = ty.split('<').next().unwrap_or(ty).trim();
    if matches!(outer, "result" | "option" | "box" | "arc" | "rc" | "vec"
        | "Result" | "Option" | "Box" | "Arc" | "Rc" | "Vec")
    {
        if let Some(start) = ty.find('<') {
            let raw = ty[start + 1..].trim_end_matches('>');
            let first = raw.split(',').next().unwrap_or("").trim();
            let first = first.strip_prefix('&').unwrap_or(first);
            let first = first.strip_prefix("mut ").unwrap_or(first);
            let inner_leaf = first.split('<').next().unwrap_or(first).trim();
            if !inner_leaf.is_empty() && inner_leaf != outer { return inner_leaf; }
        }
    }
    outer
}

pub fn owning_type(name: &str) -> Option<String> {
    let (prefix, _) = name.rsplit_once("::")?;
    let leaf = prefix.rsplit_once("::").map_or(prefix, |p| p.1);
    let leaf = leaf.rsplit_once(" for ").map_or(leaf, |(_, c)| c);
    Some(leaf.split('<').next().unwrap_or(leaf).to_lowercase())
}

pub(crate) fn strip_generics_from_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut depth = 0u32;
    for ch in key.chars() {
        match ch {
            '<' => depth += 1,
            '>' => { depth = depth.saturating_sub(1); }
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

pub fn extract_generic_bounds(sig: &str) -> Vec<(String, String)> {
    let Some(start) = sig.find('<') else { return Vec::new() };
    let mut depth = 0u32;
    let mut end = start;
    for (i, ch) in sig[start..].char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => { depth -= 1; if depth == 0 { end = start + i; break; } }
            _ => {}
        }
    }
    if end <= start + 1 { return Vec::new(); }

    let mut result = Vec::new();
    for param in split_at_commas(&sig[start + 1..end]) {
        let param = param.trim();
        if param.starts_with('\'') || param.starts_with("const ") { continue; }
        if let Some((name, bound_str)) = param.split_once(':') {
            let first = bound_str.split('+').next().unwrap_or("").trim()
                .split('<').next().unwrap_or("").trim();
            if !first.is_empty() && first != "?" {
                result.push((name.trim().to_lowercase(), first.to_lowercase()));
            }
        }
    }
    // where clause
    if let Some(where_pos) = sig.to_lowercase().find("where ") {
        for clause in sig[where_pos + 6..].split(',') {
            let clause = clause.trim().trim_end_matches('{').trim();
            if let Some((tp, bound_str)) = clause.split_once(':') {
                let tp = tp.trim().to_lowercase();
                let first = bound_str.split('+').next().unwrap_or("").trim()
                    .split('<').next().unwrap_or("").trim();
                if !first.is_empty() && !result.iter().any(|(t, _)| t == &tp) {
                    result.push((tp, first.to_lowercase()));
                }
            }
        }
    }
    result
}
pub fn is_test_path(path: &str) -> bool {
    path.contains("/tests/") || path.contains("\\tests\\")
        || path.contains("/test/") || path.contains("\\test\\")
        || path.ends_with("_test.rs") || path.ends_with("_test.go")
}

pub fn is_test_chunk(c: &ParsedChunk) -> bool {
    c.is_test || is_test_path(&c.file)
}
fn owner_leaf(name: &str) -> String {
    let lower = name.to_lowercase();
    let leaf = lower.rsplit("::").next().unwrap_or(&lower);
    let leaf = leaf.rsplit_once(" for ").map_or(leaf, |(_, c)| c);
    leaf.split('<').next().unwrap_or(leaf).to_owned()
}

fn split_at_commas(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut depth = 0u32;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => { result.push(&s[start..i]); start = i + 1; }
            _ => {}
        }
    }
    result.push(&s[start..]);
    result
}
