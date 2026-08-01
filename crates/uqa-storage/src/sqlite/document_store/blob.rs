//! Blob markers, hydration, validation, and blob-table persistence.

use super::{
    allocation_error, decode_legacy_json_value, params, sqlite_doc_id, BTreeMap, DocId, Document,
    OptionalExtension, SQLiteError, SQLiteResult, StoredValue, Value, BLOB_MARKER_ENCODING,
    BLOB_MARKER_FIELD, BLOB_MARKER_TYPE, BLOB_MARKER_VALUE, DOCUMENT_BLOBS_TABLE,
    VALUE_BLOB_F64_LIST, VALUE_BLOB_F64_TENSOR, VALUE_BLOB_MARKER_VALUE, VALUE_BLOB_TYPED_JSON,
};

/// Take `field` out of a parsed document body, hydrating a blob marker
/// into its stored bytes. `Ok(None)` means the document has no such
/// field, so the caller keeps its absent-field default.
pub(super) fn take_requested_field(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: DocId,
    document: &mut Document,
    field: &str,
) -> SQLiteResult<Option<Value>> {
    let Some(mut value) = document.remove(field) else {
        return Ok(None);
    };
    if let Some(marker) = blob_marker_info(&value) {
        if let Some(decoded) = load_marked_document_blob(conn, table, doc_id, field, &marker)? {
            value = decoded;
        }
    }
    Ok(Some(value))
}

pub(super) fn decode_json_field_value(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: DocId,
    field: &str,
    json_type: Option<String>,
    json_text: &str,
) -> SQLiteResult<Option<Value>> {
    let Some(json_type) = json_type else {
        return Ok(None);
    };
    let mut value = match json_type.as_str() {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "null" => Value::Null,
        _ => decode_legacy_json_value(serde_json::from_str(json_text)?)?,
    };
    if let Some(marker) = blob_marker_info(&value) {
        if let Some(decoded) = load_marked_document_blob(conn, table, doc_id, field, &marker)? {
            value = decoded;
        }
    }
    Ok(Some(value))
}

pub(super) fn blob_marker(field: String) -> Value {
    Value::Map(BTreeMap::from([
        (
            BLOB_MARKER_TYPE.to_string(),
            Value::Str(BLOB_MARKER_VALUE.to_string()),
        ),
        (BLOB_MARKER_FIELD.to_string(), Value::Str(field)),
    ]))
}

pub(super) fn value_blob_marker(field: String, encoding: &str) -> Value {
    Value::Map(BTreeMap::from([
        (
            BLOB_MARKER_TYPE.to_string(),
            Value::Str(VALUE_BLOB_MARKER_VALUE.to_string()),
        ),
        (BLOB_MARKER_FIELD.to_string(), Value::Str(field)),
        (
            BLOB_MARKER_ENCODING.to_string(),
            Value::Str(encoding.to_string()),
        ),
    ]))
}

#[derive(Debug, Clone)]
pub(super) enum BlobMarker {
    Bytes(String),
    F64List(String),
    F64Tensor(String),
    TypedValue(String),
}

pub(super) fn blob_marker_info(value: &Value) -> Option<BlobMarker> {
    let Value::Map(map) = value else {
        return None;
    };
    match (map.get(BLOB_MARKER_TYPE), map.get(BLOB_MARKER_FIELD)) {
        (Some(Value::Str(kind)), Some(Value::Str(field))) if kind == BLOB_MARKER_VALUE => {
            Some(BlobMarker::Bytes(field.clone()))
        }
        (Some(Value::Str(kind)), Some(Value::Str(field))) if kind == VALUE_BLOB_MARKER_VALUE => {
            match map.get(BLOB_MARKER_ENCODING) {
                Some(Value::Str(encoding)) if encoding == VALUE_BLOB_F64_LIST => {
                    Some(BlobMarker::F64List(field.clone()))
                }
                Some(Value::Str(encoding)) if encoding == VALUE_BLOB_F64_TENSOR => {
                    Some(BlobMarker::F64Tensor(field.clone()))
                }
                Some(Value::Str(encoding)) if encoding == VALUE_BLOB_TYPED_JSON => {
                    Some(BlobMarker::TypedValue(field.clone()))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

pub(super) fn hydrate_document_blobs(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: DocId,
    document: &mut Document,
) -> SQLiteResult<()> {
    let mut marker_fields = Vec::new();
    marker_fields
        .try_reserve_exact(document.len())
        .map_err(|error| allocation_error("document blob markers", error))?;
    for (field, value) in document.iter() {
        if let Some(marker) = blob_marker_info(value) {
            marker_fields.push((field.clone(), marker));
        }
    }
    for (field, marker) in marker_fields {
        if let Some(value) = load_marked_document_blob(conn, table, doc_id, &field, &marker)? {
            document.insert(field, value);
        }
    }
    Ok(())
}

pub(super) fn load_marked_document_blob(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: DocId,
    expected_field: &str,
    marker: &BlobMarker,
) -> SQLiteResult<Option<Value>> {
    let field = match marker {
        BlobMarker::Bytes(field)
        | BlobMarker::F64List(field)
        | BlobMarker::F64Tensor(field)
        | BlobMarker::TypedValue(field) => field.as_str(),
    };
    if field != expected_field {
        return Err(SQLiteError::CorruptDocumentBlob {
            table: table.to_string(),
            doc_id,
            field: expected_field.to_string(),
            reason: format!(
                "JSON marker for field `{expected_field}` references blob field `{field}`"
            ),
        });
    }
    let bytes = load_document_blob(conn, table, doc_id, field)?.ok_or_else(|| {
        SQLiteError::CorruptDocumentBlob {
            table: table.to_string(),
            doc_id,
            field: field.to_string(),
            reason: "JSON marker references a missing blob row".to_string(),
        }
    })?;
    let value = match marker {
        BlobMarker::Bytes(_) => Value::Bytes(bytes),
        BlobMarker::F64List(_) => {
            decode_f64_list_blob(&bytes)?.ok_or_else(|| SQLiteError::CorruptDocumentBlob {
                table: table.to_string(),
                doc_id,
                field: field.to_string(),
                reason: "invalid f64-list encoding".to_string(),
            })?
        }
        BlobMarker::F64Tensor(_) => {
            decode_f64_tensor_blob(&bytes)?.ok_or_else(|| SQLiteError::CorruptDocumentBlob {
                table: table.to_string(),
                doc_id,
                field: field.to_string(),
                reason: "invalid f64-tensor encoding".to_string(),
            })?
        }
        BlobMarker::TypedValue(_) => serde_json::from_slice::<StoredValue>(&bytes)
            .map(StoredValue::into_value)
            .map_err(|error| SQLiteError::CorruptDocumentBlob {
                table: table.to_string(),
                doc_id,
                field: field.to_string(),
                reason: format!("invalid typed-value encoding: {error}"),
            })?,
    };
    Ok(Some(value))
}

pub(super) fn decode_f64_list_blob(bytes: &[u8]) -> SQLiteResult<Option<Value>> {
    if bytes.len() % std::mem::size_of::<f64>() != 0 {
        return Ok(None);
    }
    let count = bytes.len() / std::mem::size_of::<f64>();
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|error| allocation_error("decoded f64-list", error))?;
    for chunk in bytes.chunks_exact(std::mem::size_of::<f64>()) {
        let mut raw = [0_u8; std::mem::size_of::<f64>()];
        raw.copy_from_slice(chunk);
        values.push(Value::Float(f64::from_le_bytes(raw)));
    }
    Ok(Some(Value::List(values)))
}

pub(super) fn decode_f64_tensor_blob(bytes: &[u8]) -> SQLiteResult<Option<Value>> {
    if bytes.len() < 8 {
        return Ok(None);
    }
    let mut rows_raw = [0_u8; 4];
    rows_raw.copy_from_slice(&bytes[0..4]);
    let rows = usize::try_from(u32::from_le_bytes(rows_raw)).map_err(|_| {
        SQLiteError::StorageBackend("tensor row count exceeds platform usize".into())
    })?;
    let mut cols_raw = [0_u8; 4];
    cols_raw.copy_from_slice(&bytes[4..8]);
    let cols = usize::try_from(u32::from_le_bytes(cols_raw)).map_err(|_| {
        SQLiteError::StorageBackend("tensor column count exceeds platform usize".into())
    })?;
    let payload = &bytes[8..];
    let Some(value_count) = rows.checked_mul(cols) else {
        return Ok(None);
    };
    let Some(payload_len) = value_count.checked_mul(std::mem::size_of::<f64>()) else {
        return Ok(None);
    };
    let Some(row_len) = cols.checked_mul(std::mem::size_of::<f64>()) else {
        return Ok(None);
    };
    if rows == 0 || cols == 0 || payload.len() != payload_len {
        return Ok(None);
    }
    let mut out = Vec::new();
    out.try_reserve_exact(rows)
        .map_err(|error| allocation_error("decoded f64-tensor rows", error))?;
    for row in payload.chunks_exact(row_len) {
        let mut values = Vec::new();
        values
            .try_reserve_exact(cols)
            .map_err(|error| allocation_error("decoded f64-tensor row", error))?;
        for chunk in row.chunks_exact(std::mem::size_of::<f64>()) {
            let mut raw = [0_u8; std::mem::size_of::<f64>()];
            raw.copy_from_slice(chunk);
            values.push(Value::Float(f64::from_le_bytes(raw)));
        }
        out.push(Value::List(values));
    }
    Ok(Some(Value::List(out)))
}

pub(super) fn load_document_blob(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: DocId,
    field: &str,
) -> SQLiteResult<Option<Vec<u8>>> {
    let sqlite_doc_id = sqlite_doc_id(doc_id)?;
    conn.query_row(
        &format!(
            "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
             WHERE table_name = ?1 AND doc_id = ?2 AND field_name = ?3"
        ),
        params![table, sqlite_doc_id, field],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

pub(super) fn delete_document_blob(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: DocId,
    field: &str,
) -> SQLiteResult<()> {
    let sqlite_doc_id = sqlite_doc_id(doc_id)?;
    conn.execute(
        &format!(
            "DELETE FROM {DOCUMENT_BLOBS_TABLE}
             WHERE table_name = ?1 AND doc_id = ?2 AND field_name = ?3"
        ),
        params![table, sqlite_doc_id, field],
    )?;
    Ok(())
}

pub(super) fn upsert_document_blob(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: DocId,
    field: &str,
    bytes: &[u8],
) -> SQLiteResult<()> {
    let sqlite_doc_id = sqlite_doc_id(doc_id)?;
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO {DOCUMENT_BLOBS_TABLE}
             (table_name, doc_id, field_name, bytes)
             VALUES (?1, ?2, ?3, ?4)"
        ),
        params![table, sqlite_doc_id, field, bytes],
    )?;
    Ok(())
}
