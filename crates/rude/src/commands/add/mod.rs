pub(crate) mod ingest;
mod run;

#[allow(unused_imports)]
pub(crate) use ingest::{ingest_mir, write_chunks, CodeChunkEntry};
pub use run::run;
