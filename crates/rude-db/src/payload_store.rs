//! PayloadStore trait — read-only access to payload + text.

use anyhow::Result;

use crate::payload::Payload;

/// Storage backend for payload data (metadata + text) associated with points.
pub trait PayloadStore {
    /// Retrieve metadata for a point.
    fn get_payload(&self, id: u64) -> Result<Option<Payload>>;

    /// Retrieve only the text chunk for a point.
    fn get_text(&self, id: u64) -> Result<Option<String>>;
}
