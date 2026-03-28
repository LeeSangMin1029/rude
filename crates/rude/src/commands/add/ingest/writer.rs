use std::collections::HashMap;

use anyhow::Result;
use rude_db::file_index::FileIndex;

use super::CodeChunkEntry;

#[tracing::instrument(skip_all)]
pub(crate) fn write_chunks(
    entries: &[CodeChunkEntry],
    file_metadata_map: &HashMap<String, (u64, u64, Vec<u64>)>,
    file_idx: &mut FileIndex,
    include_content_hash: bool,
) -> Result<u64> {
    for (path, (mtime, size, chunk_ids)) in file_metadata_map {
        let hash = if include_content_hash {
            Some(rude_util::content_hash(std::path::Path::new(path)).unwrap_or(0))
        } else {
            None
        };
        file_idx.update_file(path.to_string(), *mtime, *size, chunk_ids.clone(), hash);
    }
    Ok(entries.len() as u64)
}
