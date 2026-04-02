use std::path::{Path, PathBuf};
use anyhow::{Result, bail};
use syn::spanned::Spanned;
use super::file::{resolve_abs_path, relative_display};

pub(crate) struct SymbolLocation {
    pub(crate) abs_path: PathBuf,
    pub(crate) rel_path: String,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
    pub(crate) kind: String,
}

pub(crate) fn locate_symbol(db: &Path, symbol: &str, file_hint: Option<&str>) -> Result<SymbolLocation> {
    let graph = crate::commands::intel::load_or_build_graph()?;
    locate_symbol_in(&graph, db, symbol, file_hint)
}

pub(crate) fn locate_symbol_in(graph: &rude_intel::graph::CallGraph, db: &Path, symbol: &str, file_hint: Option<&str>) -> Result<SymbolLocation> {
    let hint_normalized = file_hint.map(|f| f.replace('\\', "/"));
    let candidates: Vec<u32> = graph.resolve(symbol).into_iter()
        .filter(|&i| {
            let cf = &graph.chunks[i as usize].file;
            hint_normalized.as_ref().is_none_or(|f| cf.ends_with(f) || f.ends_with(cf))
        })
        .collect();
    let leaf = symbol.rsplit("::").next().unwrap_or(symbol);
    if candidates.is_empty() {
        let Some(hint) = file_hint else { bail!("Symbol '{symbol}' not found (use --file to search by syn)"); };
        let file_path = graph.chunks.iter()
            .find(|c| c.file.ends_with(hint))
            .map_or(hint, |c| c.file.as_str());
        let abs_path = resolve_abs_path(db, file_path)?;
        if !abs_path.exists() { bail!("File not found: {}", abs_path.display()); }
        let rel = relative_display(db, file_path);
        let (start, end) = syn_locate(&abs_path, leaf, None)?;
        return Ok(SymbolLocation { abs_path, rel_path: rel, start_line: start - 1, end_line: end - 1, kind: "function".into() });
    }
    if candidates.len() > 1 {
        let locs: Vec<String> = candidates.iter()
            .map(|&i| { let c = &graph.chunks[i as usize]; format!("  {} [{}] {}:{}",
                c.name, c.kind, c.file, c.lines.map_or("?".into(), |(s, e)| format!("{s}-{e}"))) }).collect();
        bail!("Ambiguous '{symbol}' — {} matches:\n{}", candidates.len(), locs.join("\n"));
    }
    let chunk = &graph.chunks[candidates[0] as usize];
    let abs_path = resolve_abs_path(db, &chunk.file)?;
    let rel = relative_display(db, &chunk.file);
    let kind = chunk.kind.clone();
    // always use syn for accurate line numbers (DB cache may be stale after edits)
    let (start, end) = syn_locate(&abs_path, leaf, None)
        .unwrap_or_else(|_| chunk.lines.unwrap_or((1, 1)));
    let start = expand_to_attrs(&abs_path, start);
    Ok(SymbolLocation { abs_path, rel_path: rel, start_line: start - 1, end_line: end - 1, kind })
}

fn expand_to_attrs(path: &Path, start: usize) -> usize {
    let Ok(content) = std::fs::read_to_string(path) else { return start };
    let lines: Vec<&str> = content.lines().collect();
    let mut s = start.saturating_sub(1);
    while s > 0 {
        let prev = lines.get(s - 1).map(|l| l.trim()).unwrap_or("");
        if prev.starts_with("#[") || prev.starts_with("///") || prev.starts_with("//!") {
            s -= 1;
        } else if is_inside_multiline_attr(&lines, s - 1) {
            s -= 1;
        } else {
            break;
        }
    }
    s + 1
}
fn is_inside_multiline_attr(lines: &[&str], line_idx: usize) -> bool {
    let mut depth: i32 = 0;
    for i in (0..=line_idx).rev() {
        let t = lines[i].trim();
        depth += t.chars().filter(|&c| c == ']').count() as i32;
        depth -= t.chars().filter(|&c| c == '[').count() as i32;
        if t.starts_with("#[") {
            return depth < 0;
        }
        if t.starts_with("///") || t.starts_with("//!") || t.starts_with("#[") { continue; }
        if !t.is_empty() && !t.starts_with("//") && depth >= 0 { return false; }
    }
    false
}

fn span_line(span: proc_macro2::Span, end: bool) -> usize {
    if end { span.end().line } else { span.start().line }
}

fn syn_locate(path: &Path, name: &str, owner: Option<&str>) -> Result<(usize, usize)> {
    let content = std::fs::read_to_string(path)?;
    match syn::parse_file(&content) {
        Ok(file) => {
            let mut finder = SymbolFinder { name, owner, current_impl: None, result: None };
            syn::visit::visit_file(&mut finder, &file);
            finder.result.ok_or_else(|| anyhow::anyhow!("Symbol '{name}' not found by syn in {}", path.display()))
        }
        Err(_) => text_fallback(&content, name)
            .ok_or_else(|| anyhow::anyhow!("Symbol '{name}' not found (syn parse failed) in {}", path.display())),
    }
}

struct SymbolFinder<'a> {
    name: &'a str,
    owner: Option<&'a str>,
    current_impl: Option<String>,
    result: Option<(usize, usize)>,
}

impl<'a, 'ast> syn::visit::Visit<'ast> for SymbolFinder<'a> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if self.result.is_some() { return; }
        if self.owner.is_none() {
            if let Some(r) = item_span(item, self.name) { self.result = Some(r); return; }
        }
        syn::visit::visit_item(self, item);
    }
    fn visit_item_impl(&mut self, imp: &'ast syn::ItemImpl) {
        let prev = self.current_impl.take();
        let ty_str = imp.self_ty.span().source_text().unwrap_or_default();
        self.current_impl = Some(ty_str.split('<').next().unwrap_or(&ty_str).trim().to_owned());
        syn::visit::visit_item_impl(self, imp);
        self.current_impl = prev;
    }
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if self.result.is_none() && f.sig.ident == self.name
            && self.owner.map_or(true, |o| self.current_impl.as_deref() == Some(o))
        {
            self.result = Some((attr_start(&f.attrs, f.sig.fn_token.span), span_line(f.block.brace_token.span.close(), true)));
        }
    }
}

fn text_fallback(content: &str, name: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    let patterns = [format!("fn {name}"), format!("struct {name}"), format!("enum {name}"), format!("const {name}")];
    let start = lines.iter().position(|l| patterns.iter().any(|p| l.contains(p.as_str())))?;
    let mut depth: i32 = 0;
    let mut end = start;
    for (i, line) in lines[start..].iter().enumerate() {
        depth += line.chars().filter(|&c| c == '{').count() as i32;
        depth -= line.chars().filter(|&c| c == '}').count() as i32;
        if depth <= 0 && i > 0 { end = start + i; break; }
    }
    if end == start { end = lines.len().saturating_sub(1).max(start); }
    Some((start + 1, end + 1))
}

fn attr_start(attrs: &[syn::Attribute], fallback: proc_macro2::Span) -> usize {
    if attrs.is_empty() { span_line(fallback, false) } else { span_line(attrs[0].span(), false) }
}

fn item_span(item: &syn::Item, name: &str) -> Option<(usize, usize)> {
    match item {
        syn::Item::Fn(f) if f.sig.ident == name =>
            Some((attr_start(&f.attrs, f.sig.fn_token.span), span_line(f.block.brace_token.span.close(), true))),
        syn::Item::Struct(s) if s.ident == name => {
            let start = attr_start(&s.attrs, s.struct_token.span);
            let end = match &s.fields {
                syn::Fields::Named(n) => span_line(n.brace_token.span.close(), true),
                syn::Fields::Unnamed(u) => span_line(u.paren_token.span.close(), true),
                syn::Fields::Unit => s.semi_token.map(|t| span_line(t.span, false)).unwrap_or(start),
            };
            Some((start, end))
        }
        syn::Item::Enum(e) if e.ident == name =>
            Some((attr_start(&e.attrs, e.enum_token.span), span_line(e.brace_token.span.close(), true))),
        syn::Item::Trait(t) if t.ident == name =>
            Some((attr_start(&t.attrs, t.trait_token.span), span_line(t.brace_token.span.close(), true))),
        syn::Item::Const(c) if c.ident == name =>
            Some((attr_start(&c.attrs, c.const_token.span), span_line(c.semi_token.span, false))),
        syn::Item::Static(s) if s.ident == name =>
            Some((attr_start(&s.attrs, s.static_token.span), span_line(s.semi_token.span, false))),
        syn::Item::Type(t) if t.ident == name =>
            Some((attr_start(&t.attrs, t.type_token.span), span_line(t.semi_token.span, false))),
        _ => None,
    }
}
