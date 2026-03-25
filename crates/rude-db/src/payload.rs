use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payload {
    pub source: String,
    pub tags: Vec<String>,
    pub created_at: u64,
    pub source_modified_at: u64,
    pub chunk_index: u32,
    pub chunk_total: u32,
    pub custom: HashMap<String, PayloadValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PayloadValue {
    String(String),
    Integer(i64),
    Float(f64),
    Bool(bool),
    StringList(Vec<String>),
}
