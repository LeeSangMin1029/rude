//! Adapter layer for rustc_private API.
//! All rustc_* imports live here — nightly changes only affect this file.

pub use rustc_hir::def::DefKind;
pub use rustc_hir::ItemKind;
pub use rustc_middle::mir::TerminatorKind;
pub use rustc_middle::ty::TyCtxt;
pub use rustc_span::def_id::{DefId, LOCAL_CRATE};

/// Consistent name for a DefId.
///
/// Uses `def_path_str` (the standard rustc display name). For local items
/// this gives the raw definition path; for external items it may use
/// re-export visible paths, which edge_resolve handles via crate prefix
/// stripping and suffix matching.
pub fn canonical_name(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    tcx.def_path_str(def_id)
}

/// Extract a filename string from a span via the source map.
pub fn extract_filename(
    source_map: &rustc_span::source_map::SourceMap,
    span: rustc_span::Span,
) -> String {
    match source_map.span_to_filename(span) {
        rustc_span::FileName::Real(ref name) => {
            let path_str = format!("{name:?}");
            if let Some(start) = path_str.find("name: \"") {
                let rest = &path_str[start + 7..];
                if let Some(end) = rest.find('"') {
                    return rest[..end].replace("\\\\", "/").to_string();
                }
            }
            path_str
        }
        other => format!("{other:?}"),
    }
}

/// Extract visibility string from `tcx.visibility(def_id)`.
pub fn extract_visibility(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    let vis = tcx.visibility(def_id);
    if vis.is_public() {
        "pub".to_string()
    } else {
        let vis_str = format!("{vis:?}");
        if vis_str.contains("Restricted") {
            if let rustc_middle::ty::Visibility::Restricted(restricted_id) = vis {
                if restricted_id
                    == tcx
                        .parent_module_from_def_id(def_id.expect_local())
                        .to_def_id()
                {
                    String::new()
                } else if restricted_id == LOCAL_CRATE.as_def_id() {
                    "pub(crate)".to_string()
                } else {
                    format!("pub(in {})", tcx.def_path_str(restricted_id))
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    }
}
