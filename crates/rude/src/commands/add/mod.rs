pub(crate) mod ingest;
pub mod run;

#[allow(unused_imports)]
pub(crate) use ingest::{ingest_mir, write_chunks, CodeChunkEntry};
pub use run::run;
