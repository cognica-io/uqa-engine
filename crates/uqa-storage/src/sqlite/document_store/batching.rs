//! Checked document-id conversion and probe-vs-scan batch construction.

use super::{DocId, SQLiteError, SQLiteResult};

/// Batch size for `doc_id IN (...)` reads. Partial batches are padded
/// by repeating the final id so every batch reuses one cached
/// statement text.
const DOC_ID_IN_CHUNK: usize = 256;

pub(super) fn sqlite_doc_id(doc_id: DocId) -> SQLiteResult<i64> {
    i64::try_from(doc_id).map_err(|_| {
        SQLiteError::StorageBackend(format!("document id {doc_id} exceeds SQLite INTEGER"))
    })
}

pub(super) fn document_id_from_sqlite(raw: i64) -> SQLiteResult<DocId> {
    DocId::try_from(raw).map_err(|_| {
        SQLiteError::StorageBackend(format!("negative SQLite document id {raw} is invalid"))
    })
}

pub(super) fn read_doc_id(row: &rusqlite::Row<'_>, index: usize) -> SQLiteResult<DocId> {
    document_id_from_sqlite(row.get::<_, i64>(index)?)
}

pub(super) fn allocation_error(context: &str, error: impl std::fmt::Display) -> SQLiteError {
    SQLiteError::StorageBackend(format!("cannot allocate {context}: {error}"))
}

/// Build `?first,?first+1,...` placeholders for a doc-id IN clause.
pub(super) fn doc_id_in_placeholders(first_index: usize, count: usize) -> SQLiteResult<String> {
    let estimated_capacity = count.checked_mul(5).ok_or_else(|| {
        SQLiteError::StorageBackend("document-id placeholder capacity overflow".into())
    })?;
    let mut out = String::new();
    out.try_reserve(estimated_capacity)
        .map_err(|error| allocation_error("document-id placeholders", error))?;
    for i in 0..count {
        if i > 0 {
            out.push(',');
        }
        out.push('?');
        let parameter_index = first_index.checked_add(i).ok_or_else(|| {
            SQLiteError::StorageBackend("document-id parameter index overflow".into())
        })?;
        out.push_str(&parameter_index.to_string());
    }
    Ok(out)
}

/// Bind values for one padded id batch: leading params (table name,
/// optional JSON path) followed by exactly `DOC_ID_IN_CHUNK` ids.
pub(super) fn chunk_bind_values(
    leading: &[rusqlite::types::Value],
    chunk: &[DocId],
) -> SQLiteResult<Vec<rusqlite::types::Value>> {
    let capacity = leading
        .len()
        .checked_add(DOC_ID_IN_CHUNK)
        .ok_or_else(|| SQLiteError::StorageBackend("document-id bind count overflow".into()))?;
    let mut bind = Vec::new();
    bind.try_reserve_exact(capacity)
        .map_err(|error| allocation_error("document-id bind values", error))?;
    bind.extend_from_slice(leading);
    let pad = chunk.last().copied().unwrap_or(0);
    for i in 0..DOC_ID_IN_CHUNK {
        let id = chunk.get(i).copied().unwrap_or(pad);
        bind.push(rusqlite::types::Value::Integer(sqlite_doc_id(id)?));
    }
    Ok(bind)
}

pub(super) fn should_probe_doc_ids(requested: usize, total: usize) -> bool {
    requested
        .checked_mul(2)
        .is_some_and(|doubled| doubled < total)
}

pub(super) fn sorted_unique_doc_ids(doc_ids: &[DocId]) -> SQLiteResult<Vec<DocId>> {
    let mut requested = Vec::new();
    requested
        .try_reserve_exact(doc_ids.len())
        .map_err(|error| allocation_error("requested document ids", error))?;
    requested.extend_from_slice(doc_ids);
    requested.sort_unstable();
    requested.dedup();
    Ok(requested)
}
