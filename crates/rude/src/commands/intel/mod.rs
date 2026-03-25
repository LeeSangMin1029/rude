
mod cluster;
mod coverage;
mod dead;
mod query;
#[cfg(test)]
mod tests;

pub(crate) use query::{load_or_build_graph, load_or_build_graph_with_chunks};
pub use cluster::run_cluster;
pub use coverage::run_coverage;
pub use dead::run_dead;
pub use query::{run_aliases, run_context, run_stats, run_symbols, run_trace};

#[cfg(test)]
pub use rude_intel::{context, parse};
