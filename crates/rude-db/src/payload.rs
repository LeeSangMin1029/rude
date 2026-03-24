//! Payload and PayloadValue types (mirrors v-hnsw-core Payload API).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Arbitrary key-value metadata associated with a point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payload {
    /// Source document path (e.g., "notes/2024-01-15-meeting.md").
    pub source: String,
    /// Tags for filtering.
    pub tags: Vec<String>,
    /// Unix timestamp when the chunk was created.
    pub created_at: u64,
    /// Unix timestamp of the source document's last modification.
    pub source_modified_at: u64,
    /// Chunk index within the source document (0-based).
    pub chunk_index: u32,
    /// Total number of chunks from this source document.
    pub chunk_total: u32,
    /// Arbitrary user-defined key-value pairs.
    pub custom: HashMap<String, PayloadValue>,
}

/// A typed value in the custom metadata map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PayloadValue {
    String(String),
    Integer(i64),
    Float(f64),
    Bool(bool),
    StringList(Vec<String>),
}
