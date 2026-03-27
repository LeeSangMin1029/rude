use std::path::Path;
use anyhow::{Context, Result};

pub fn content_hash(path: &Path) -> Result<u64> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read file for hashing: {}", path.display()))?;
    Ok(content_hash_bytes(&bytes))
}

pub fn content_hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

pub fn generate_id(source: &str, chunk_index: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    chunk_index.hash(&mut hasher);
    hasher.finish()
}
