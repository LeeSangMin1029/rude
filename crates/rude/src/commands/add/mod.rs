//! Code add/update: chunks files via MIR, stores text + payload (no embedding).

pub(crate) mod ingest;
mod run;

pub(crate) use ingest::{build_callers, build_payload, ingest_mir, CodeChunkEntry};
pub use run::run;
