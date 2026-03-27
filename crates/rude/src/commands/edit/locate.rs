use std::path::{Path, PathBuf};
use anyhow::{Result, bail};
use super::path::{resolve_abs_path, relative_display};

pub(crate) struct SymbolLocation {
    pub(crate) abs_path: PathBuf,
    pub(crate) rel_path: String,
    pub(crate) start_line: usize,
    pub(crate) end_line: usize,
}

pub(crate) fn locate_symbol(db: &Path, symbol: &str, file_hint: Option<&str>) -> Result<SymbolLocation> {
    let graph = crate::commands::intel::load_or_build_graph()?;
    let indices = graph.resolve(symbol);
    let candidates: Vec<u32> = indices.into_iter()
        .filter(|&i| file_hint.is_none_or(|f| graph.chunks[i as usize].file.ends_with(f)))
        .collect();
    let leaf = symbol.rsplit("::").next().unwrap_or(symbol);
    if candidates.is_empty() {
        let Some(hint) = file_hint else { bail!("Symbol '{symbol}' not found (use --file to search by syn)"); };
        let matching_file = graph.chunks.iter()
            .find(|c| c.file.ends_with(hint))
            .map(|c| c.file.clone());
        let file_path = matching_file.as_deref().unwrap_or(hint);
        let abs_path = resolve_abs_path(db, file_path)?;
        if !abs_path.exists() { bail!("File not found: {}", abs_path.display()); }
        let rel = relative_display(db, file_path);
        let (start, end) = syn_locate(&abs_path, leaf, None)?;
        return Ok(SymbolLocation { abs_path, rel_path: rel, start_line: start, end_line: end });
    }
    if candidates.len() > 1 {
        let locs: Vec<String> = candidates.iter()
            .map(|&i| { let c = &graph.chunks[i as usize]; format!("  {} [{}] {}:{}", c.name, c.kind,
                c.file, c.lines.map_or("?".into(), |(s, e)| format!("{s}-{e}"))) }).collect();
        bail!("Ambiguous '{symbol}' — {} matches:\n{}", candidates.len(), locs.join("\n"));
    }
    let chunk = &graph.chunks[candidates[0] as usize];
    let abs_path = resolve_abs_path(db, &chunk.file)?;
    let rel = relative_display(db, &chunk.file);
    let owner = extract_owner(&chunk.name);
    let (start, end) = syn_locate(&abs_path, leaf, owner.as_deref())?;
    Ok(SymbolLocation { abs_path, rel_path: rel, start_line: start, end_line: end })
}

fn extract_owner(full_path: &str) -> Option<String> {
    let parts: Vec<&str> = full_path.rsplitn(3, "::").collect();
    if parts.len() >= 2 {
        let candidate = parts[1];
        if candidate.chars().next().is_some_and(|c| c.is_uppercase()) {
            return Some(candidate.to_owned());
        }
    }
    None
}

fn line_of(span: proc_macro2::Span) -> usize {
    span.start().line.saturating_sub(1)
}

fn end_line_of(span: proc_macro2::Span) -> usize {
    span.end().line.saturating_sub(1)
}

fn syn_locate(path: &Path, name: &str, owner: Option<&str>) -> Result<(usize, usize)> {
    let content = std::fs::read_to_string(path)?;
    match syn::parse_file(&content) {
        Ok(file) => {
            let mut finder = SymbolFinder::new(name, owner);
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

impl<'a> SymbolFinder<'a> {
    fn new(name: &'a str, owner: Option<&'a str>) -> Self {
        Self { name, owner, current_impl: None, result: None }
    }
    fn matches_owner(&self) -> bool {
        match self.owner {
            None => true,
            Some(o) => self.current_impl.as_ref().is_some_and(|c| c == o),
        }
    }
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
        use syn::spanned::Spanned;
        let prev = self.current_impl.take();
        let ty_str = imp.self_ty.span().source_text().unwrap_or_default();
        self.current_impl = Some(ty_str.split('<').next().unwrap_or(&ty_str).trim().to_owned());
        syn::visit::visit_item_impl(self, imp);
        self.current_impl = prev;
    }
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if self.result.is_none() && f.sig.ident == self.name && self.matches_owner() {
            self.result = Some((item_start(&f.attrs, f.sig.fn_token.span), end_line_of(f.block.brace_token.span.close())));
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
        for ch in line.chars() {
            if ch == '{' { depth += 1; }
            if ch == '}' { depth -= 1; }
        }
        if depth <= 0 && i > 0 { end = start + i; break; }
    }
    if end == start { end = lines.len().saturating_sub(1).max(start); }
    Some((start, end))
}

fn item_start(attrs: &[syn::Attribute], fallback: proc_macro2::Span) -> usize {
    use syn::spanned::Spanned;
    if attrs.is_empty() { line_of(fallback) } else { line_of(attrs[0].span()) }
}

fn item_span(item: &syn::Item, name: &str) -> Option<(usize, usize)> {
    match item {
        syn::Item::Fn(f) if f.sig.ident == name =>
            Some((item_start(&f.attrs, f.sig.fn_token.span), end_line_of(f.block.brace_token.span.close()))),
        syn::Item::Struct(s) if s.ident == name => {
            let start = item_start(&s.attrs, s.struct_token.span);
            let end = match &s.fields {
                syn::Fields::Named(n) => end_line_of(n.brace_token.span.close()),
                syn::Fields::Unnamed(u) => end_line_of(u.paren_token.span.close()),
                syn::Fields::Unit => s.semi_token.map(|t| line_of(t.span)).unwrap_or(start),
            };
            Some((start, end))
        }
        syn::Item::Enum(e) if e.ident == name =>
            Some((item_start(&e.attrs, e.enum_token.span), end_line_of(e.brace_token.span.close()))),
        syn::Item::Trait(t) if t.ident == name =>
            Some((item_start(&t.attrs, t.trait_token.span), end_line_of(t.brace_token.span.close()))),
        syn::Item::Const(c) if c.ident == name =>
            Some((item_start(&c.attrs, c.const_token.span), line_of(c.semi_token.span))),
        syn::Item::Static(s) if s.ident == name =>
            Some((item_start(&s.attrs, s.static_token.span), line_of(s.semi_token.span))),
        syn::Item::Type(t) if t.ident == name =>
            Some((item_start(&t.attrs, t.type_token.span), line_of(t.semi_token.span))),
        _ => None,
    }
}
