
use std::collections::HashSet;

use crate::graph::build::CallGraph;
use crate::graph::context_cmd::is_derived_noise;
use rude_util::{apply_alias, format_lines_opt, relative_path};

pub struct FlowNode {
    pub idx: u32,
    pub children: Vec<FlowNode>,
    pub backreference: bool,
}

pub fn build_flow_tree(graph: &CallGraph, seeds: &[u32], max_depth: u32, skip_test: bool) -> Vec<FlowNode> {
    let mut expanded = HashSet::new();
    seeds
        .iter()
        .map(|&idx| {
            expanded.insert(idx);
            build_subtree(graph, idx, max_depth, 0, &mut expanded, skip_test)
        })
        .collect()
}

fn build_subtree(
    graph: &CallGraph,
    idx: u32,
    max_depth: u32,
    current_depth: u32,
    expanded: &mut HashSet<u32>,
    skip_test: bool,
) -> FlowNode {
    if current_depth >= max_depth {
        return FlowNode { idx, children: Vec::new(), backreference: false };
    }

    let callees = graph.callees.get(idx as usize).map_or(&[][..], |v| v.as_slice());
    let children: Vec<FlowNode> = callees
        .iter()
        .filter(|&&child_idx| {
            let ci = child_idx as usize;
            (!skip_test || !graph.is_test.get(ci).copied().unwrap_or(false))
                && !is_derived_noise(&graph.chunks[ci].name)
        })
        .map(|&child_idx| {
            if expanded.contains(&child_idx) {
                FlowNode { idx: child_idx, children: Vec::new(), backreference: true }
            } else {
                expanded.insert(child_idx);
                build_subtree(graph, child_idx, max_depth, current_depth + 1, expanded, skip_test)
            }
        })
        .collect();

    FlowNode { idx, children, backreference: false }
}

pub fn render_tree(
    graph: &CallGraph,
    nodes: &[FlowNode],
    alias_map: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut buf = String::new();
    for node in nodes {
        let i = node.idx as usize;
        let name = &graph.chunks[i].name;
        let file = relative_path(&graph.chunks[i].file);
        let short = apply_alias(file, alias_map);
        let lines = format_lines_opt(graph.chunks[i].lines);
        let test_marker = if graph.is_test[i] { " [test]" } else { "" };
        buf.push_str(&format!("{short}{lines}  {name}{test_marker}\n"));
        render_children(graph, &node.children, &mut buf, "  ", alias_map);
    }
    buf
}

fn render_children(
    graph: &CallGraph,
    children: &[FlowNode],
    buf: &mut String,
    prefix: &str,
    alias_map: &std::collections::BTreeMap<String, String>,
) {
    let count = children.len();
    for (ci, child) in children.iter().enumerate() {
        let is_last = ci == count - 1;
        let connector = if is_last { "\u{2514}\u{2500}\u{2192} " } else { "\u{251c}\u{2500}\u{2192} " };
        let extension = if is_last { "    " } else { "\u{2502}   " };

        let i = child.idx as usize;
        let name = &graph.chunks[i].name;
        let file = relative_path(&graph.chunks[i].file);
        let short = apply_alias(file, alias_map);
        let lines = format_lines_opt(graph.chunks[i].lines);
        let test_marker = if graph.is_test[i] { " [test]" } else { "" };
        let backref = if child.backreference { "  \u{21a9}" } else { "" };

        buf.push_str(&format!("{prefix}{connector}{short}{lines}  {name}{test_marker}{backref}\n"));

        if !child.children.is_empty() {
            let next_prefix = format!("{prefix}{extension}");
            render_children(graph, &child.children, buf, &next_prefix, alias_map);
        }
    }
}
