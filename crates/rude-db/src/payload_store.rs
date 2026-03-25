use anyhow::Result;

use crate::payload::Payload;

pub trait PayloadStore {
    fn get_payload(&self, id: u64) -> Result<Option<Payload>>;
    fn get_text(&self, id: u64) -> Result<Option<String>>;
}
