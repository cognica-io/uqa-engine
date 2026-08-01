//! Checked document-id, ordinal, and vector-blob encoding.

use super::{DocId, SQLiteError, SQLiteResult};

pub(super) fn encode_doc_id(doc_id: DocId) -> SQLiteResult<i64> {
    i64::try_from(doc_id).map_err(|_| {
        SQLiteError::StorageBackend(format!(
            "document id {doc_id} does not fit in SQLite INTEGER"
        ))
    })
}

pub(super) fn decode_doc_id(doc_id: i64) -> SQLiteResult<DocId> {
    DocId::try_from(doc_id).map_err(|_| {
        SQLiteError::StorageBackend(format!(
            "invalid negative document id {doc_id} in persisted vector index"
        ))
    })
}

pub(super) fn vector_to_blob(v: &[f32]) -> SQLiteResult<Vec<u8>> {
    let capacity = v
        .len()
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| SQLiteError::StorageBackend("vector payload size overflow".into()))?;
    let mut buf = Vec::with_capacity(capacity);
    for x in v {
        buf.extend_from_slice(&x.to_le_bytes());
    }
    Ok(buf)
}

pub(super) fn usize_to_u64(field: &str, value: usize) -> SQLiteResult<u64> {
    u64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("{field} does not fit in the u64 counter range"))
    })
}

pub(super) fn validate_vector_ordinal_count(count: u64) -> SQLiteResult<()> {
    if count > u64::from(u32::MAX) + 1 {
        return Err(SQLiteError::StorageBackend(
            "vector ordinal exceeds the u32 index format".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_persisted_ordinal_sequence(
    rows: &[(DocId, u32, Vec<f32>)],
) -> SQLiteResult<()> {
    let mut current_doc = None;
    let mut expected = 0_u64;
    for (doc_id, ordinal, _) in rows {
        if current_doc != Some(*doc_id) {
            current_doc = Some(*doc_id);
            expected = 0;
        }
        if u64::from(*ordinal) != expected {
            return Err(SQLiteError::StorageBackend(format!(
                "invalid persisted vector ordinal sequence for document {doc_id}: expected {expected}, found {ordinal}"
            )));
        }
        expected = expected.checked_add(1).ok_or_else(|| {
            SQLiteError::StorageBackend("persisted vector ordinal sequence overflow".into())
        })?;
    }
    Ok(())
}

pub(super) fn blob_to_vector(blob: &[u8]) -> SQLiteResult<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return Err(SQLiteError::StorageBackend(
            "invalid vector payload".to_string(),
        ));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}
