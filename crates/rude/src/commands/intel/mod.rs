//! Code intelligence commands — structural queries on code-chunked databases.

mod commands;
#[cfg(test)]
mod tests;

pub(crate) use commands::{load_or_build_graph, load_or_build_graph_with_chunks};
pub use commands::{run_aliases, run_cluster, run_context, run_coverage, run_dead, run_stats, run_symbols, run_trace};

#[cfg(test)]
pub use rude_intel::{context, parse};
