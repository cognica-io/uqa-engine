//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed [`DocumentStore`].

use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use uqa_core::{DecimalValue, DocId, TemporalValue, Value};

use crate::backend::StorageBackendResult;
use crate::document_store::{Document, DocumentStore};
use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult, SQLiteError};

const DOCUMENT_BLOBS_TABLE: &str = "_document_blobs";
const BLOB_MARKER_TYPE: &str = "$uqa_type";
const BLOB_MARKER_VALUE: &str = "document_blob";
const BLOB_MARKER_FIELD: &str = "field";
const BLOB_MARKER_ENCODING: &str = "encoding";
const VALUE_BLOB_MARKER_VALUE: &str = "value_blob";
const VALUE_BLOB_F64_LIST: &str = "f64_list";
const VALUE_BLOB_F64_TENSOR: &str = "f64_tensor";
const VALUE_BLOB_TYPED_JSON: &str = "typed_json_v1";
const MIN_NUMERIC_BLOB_VALUES: usize = 32;

type EncodedDocument = (Document, Vec<(String, Vec<u8>)>);

/// Batch size for `doc_id IN (...)` reads. Partial batches are padded
/// by repeating the final id so every batch reuses one cached
/// statement text.
const DOC_ID_IN_CHUNK: usize = 256;

fn sqlite_doc_id(doc_id: DocId) -> SQLiteResult<i64> {
    i64::try_from(doc_id).map_err(|_| {
        SQLiteError::StorageBackend(format!("document id {doc_id} exceeds SQLite INTEGER"))
    })
}

fn document_id_from_sqlite(raw: i64) -> SQLiteResult<DocId> {
    DocId::try_from(raw).map_err(|_| {
        SQLiteError::StorageBackend(format!("negative SQLite document id {raw} is invalid"))
    })
}

fn read_doc_id(row: &rusqlite::Row<'_>, index: usize) -> SQLiteResult<DocId> {
    document_id_from_sqlite(row.get::<_, i64>(index)?)
}

fn allocation_error(context: &str, error: impl std::fmt::Display) -> SQLiteError {
    SQLiteError::StorageBackend(format!("cannot allocate {context}: {error}"))
}

/// Build `?first,?first+1,...` placeholders for a doc-id IN clause.
fn doc_id_in_placeholders(first_index: usize, count: usize) -> SQLiteResult<String> {
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
fn chunk_bind_values(
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

fn should_probe_doc_ids(requested: usize, total: usize) -> bool {
    requested
        .checked_mul(2)
        .is_some_and(|doubled| doubled < total)
}

fn sorted_unique_doc_ids(doc_ids: &[DocId]) -> SQLiteResult<Vec<DocId>> {
    let mut requested = Vec::new();
    requested
        .try_reserve_exact(doc_ids.len())
        .map_err(|error| allocation_error("requested document ids", error))?;
    requested.extend_from_slice(doc_ids);
    requested.sort_unstable();
    requested.dedup();
    Ok(requested)
}

#[derive(Clone)]
pub struct SQLiteDocumentStore {
    conn: ManagedConnection,
    table: String,
}

impl SQLiteDocumentStore {
    pub fn new(conn: ManagedConnection, table: impl Into<String>) -> Self {
        Self {
            conn,
            table: table.into(),
        }
    }

    pub fn max_doc_id(&self) -> StorageBackendResult<DocId> {
        Ok(self.conn.with(|c| {
            let id: Option<i64> = c
                .prepare_cached("SELECT MAX(doc_id) FROM _documents WHERE table_name = ?1")?
                .query_row(params![self.table], |r| r.get(0))?;
            id.map_or(Ok(0), document_id_from_sqlite)
        })?)
    }

    fn put_inner(&self, doc_id: DocId, document: &Document) -> SQLiteResult<()> {
        let sqlite_doc_id = sqlite_doc_id(doc_id)?;
        let document: Document = document
            .iter()
            .filter(|(_, value)| !matches!(value, uqa_core::Value::Null))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let (document, blobs) = encode_document_blobs(document)?;
        let body = serde_json::to_string(&document)?;
        self.conn.with(|c| {
            c.prepare_cached(&format!(
                "DELETE FROM {DOCUMENT_BLOBS_TABLE}
                 WHERE table_name = ?1 AND doc_id = ?2"
            ))?
            .execute(params![self.table, sqlite_doc_id])?;
            c.prepare_cached(
                "INSERT OR REPLACE INTO _documents (table_name, doc_id, body)
                 VALUES (?1, ?2, ?3)",
            )?
            .execute(params![self.table, sqlite_doc_id, body])?;
            for (field, bytes) in blobs {
                c.prepare_cached(&format!(
                    "INSERT OR REPLACE INTO {DOCUMENT_BLOBS_TABLE}
                     (table_name, doc_id, field_name, bytes)
                     VALUES (?1, ?2, ?3, ?4)"
                ))?
                .execute(params![self.table, sqlite_doc_id, field, bytes])?;
            }
            Ok(())
        })
    }

    fn get_inner(&self, doc_id: DocId) -> SQLiteResult<Option<Document>> {
        let sqlite_doc_id = sqlite_doc_id(doc_id)?;
        self.conn.with(|c| {
            let body: Option<String> = c
                .prepare_cached(
                    "SELECT body FROM _documents
                     WHERE table_name = ?1 AND doc_id = ?2",
                )?
                .query_row(params![self.table, sqlite_doc_id], |r| r.get(0))
                .optional()?;
            let Some(body) = body else {
                return Ok(None);
            };
            let mut document = decode_legacy_document_body(&body)?;
            hydrate_document_blobs(c, &self.table, doc_id, &mut document)?;
            Ok(Some(document))
        })
    }

    fn get_field_inner(&self, doc_id: DocId, field: &str) -> SQLiteResult<Option<Value>> {
        let sqlite_doc_id = sqlite_doc_id(doc_id)?;
        let path = sqlite_json_path(field);
        self.conn.with(|c| {
            let row: Option<(Option<String>, String)> = c
                .prepare_cached(
                    "SELECT json_type(body, ?3), json_quote(json_extract(body, ?3))
                     FROM _documents
                     WHERE table_name = ?1 AND doc_id = ?2",
                )?
                .query_row(params![self.table, sqlite_doc_id, path], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            let Some((json_type, json_text)) = row else {
                return Ok(None);
            };
            decode_json_field_value(c, &self.table, doc_id, field, json_type, &json_text)
        })
    }

    fn find_doc_id_by_field_inner(
        &self,
        field: &str,
        value: &Value,
    ) -> SQLiteResult<Option<DocId>> {
        let path = sqlite_json_path(field);
        match value {
            Value::Str(value) => self
                .conn
                .with(|c| find_doc_id_by_scalar(c, &self.table, &path, value)),
            Value::Int(value) => self
                .conn
                .with(|c| find_doc_id_by_scalar(c, &self.table, &path, value)),
            Value::Float(value) if value.is_finite() => self
                .conn
                .with(|c| find_doc_id_by_scalar(c, &self.table, &path, value)),
            Value::Bool(value) => self.conn.with(|c| {
                let json_type = if *value { "true" } else { "false" };
                let doc_id: Option<i64> = c
                    .query_row(
                        "SELECT doc_id FROM _documents
                         WHERE table_name = ?1 AND json_type(body, ?2) = ?3
                         ORDER BY doc_id LIMIT 1",
                        params![self.table, path, json_type],
                        |r| r.get(0),
                    )
                    .optional()?;
                doc_id.map(document_id_from_sqlite).transpose()
            }),
            _ => {
                let doc_ids = self.conn.with(|c| {
                    let mut stmt = c.prepare_cached(
                        "SELECT doc_id FROM _documents
                         WHERE table_name = ?1 ORDER BY doc_id",
                    )?;
                    let rows = stmt.query_map(params![self.table], |row| row.get::<_, i64>(0))?;
                    let mut out = Vec::new();
                    for row in rows {
                        out.push(document_id_from_sqlite(row?)?);
                    }
                    Ok(out)
                })?;
                for doc_id in doc_ids {
                    if self.get_field_inner(doc_id, field)?.as_ref() == Some(value) {
                        return Ok(Some(doc_id));
                    }
                }
                Ok(None)
            }
        }
    }

    fn patch_fields_inner(
        &self,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
    ) -> SQLiteResult<bool> {
        if updates.is_empty() {
            return Ok(true);
        }
        let sqlite_doc_id = sqlite_doc_id(doc_id)?;
        self.conn.with(|c| {
            let exists: Option<i64> = c
                .prepare_cached(
                    "SELECT 1 FROM _documents
                     WHERE table_name = ?1 AND doc_id = ?2
                     LIMIT 1",
                )?
                .query_row(params![self.table, sqlite_doc_id], |r| r.get(0))
                .optional()?;
            if exists.is_none() {
                return Ok(false);
            }

            for (field, value) in updates {
                let path = sqlite_json_path(field);
                match value {
                    Value::Null => {
                        delete_document_blob(c, &self.table, doc_id, field)?;
                        c.execute(
                            "UPDATE _documents SET body = json_remove(body, ?3)
                             WHERE table_name = ?1 AND doc_id = ?2",
                            params![self.table, sqlite_doc_id, path],
                        )?;
                    }
                    Value::Bytes(bytes) => {
                        let marker = serde_json::to_string(&blob_marker(field.clone()))?;
                        c.execute(
                            "UPDATE _documents SET body = json_set(body, ?3, json(?4))
                             WHERE table_name = ?1 AND doc_id = ?2",
                            params![self.table, sqlite_doc_id, path, marker],
                        )?;
                        upsert_document_blob(c, &self.table, doc_id, field, bytes)?;
                    }
                    other => {
                        let (stored, blob) = encode_stored_value(field, other.clone())?;
                        let json = serde_json::to_string(&stored)?;
                        c.execute(
                            "UPDATE _documents SET body = json_set(body, ?3, json(?4))
                             WHERE table_name = ?1 AND doc_id = ?2",
                            params![self.table, sqlite_doc_id, path, json],
                        )?;
                        if let Some(bytes) = blob {
                            upsert_document_blob(c, &self.table, doc_id, field, &bytes)?;
                        } else {
                            delete_document_blob(c, &self.table, doc_id, field)?;
                        }
                    }
                }
            }
            Ok(true)
        })
    }
}

fn sqlite_json_path(field: &str) -> String {
    if field.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
        && field
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
    {
        format!("$.{field}")
    } else {
        format!("$.{}", serde_json::Value::String(field.to_string()))
    }
}

fn find_doc_id_by_scalar<T: rusqlite::ToSql>(
    conn: &rusqlite::Connection,
    table: &str,
    path: &str,
    value: &T,
) -> SQLiteResult<Option<DocId>> {
    let doc_id: Option<i64> = conn
        .query_row(
            "SELECT doc_id FROM _documents
             WHERE table_name = ?1 AND json_extract(body, ?2) = ?3
             ORDER BY doc_id LIMIT 1",
            (table, path, value),
            |r| r.get(0),
        )
        .optional()?;
    doc_id.map(document_id_from_sqlite).transpose()
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum StoredValue {
    Null,
    Bool(bool),
    Int(i64),
    FloatBits(u64),
    Str(String),
    Bytes(Vec<u8>),
    Temporal(TemporalValue),
    Decimal(DecimalValue),
    List(Vec<StoredValue>),
    Map(BTreeMap<String, StoredValue>),
}

impl StoredValue {
    fn from_value(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(value),
            Value::Int(value) => Self::Int(value),
            Value::Float(value) => Self::FloatBits(value.to_bits()),
            Value::Str(value) => Self::Str(value),
            Value::Bytes(value) => Self::Bytes(value),
            Value::Temporal(value) => Self::Temporal(value),
            Value::Decimal(value) => Self::Decimal(value),
            Value::List(values) => Self::List(values.into_iter().map(Self::from_value).collect()),
            Value::Map(values) => Self::Map(
                values
                    .into_iter()
                    .map(|(key, value)| (key, Self::from_value(value)))
                    .collect(),
            ),
        }
    }

    fn into_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(value),
            Self::Int(value) => Value::Int(value),
            Self::FloatBits(value) => Value::Float(f64::from_bits(value)),
            Self::Str(value) => Value::Str(value),
            Self::Bytes(value) => Value::Bytes(value),
            Self::Temporal(value) => Value::Temporal(value),
            Self::Decimal(value) => Value::Decimal(value),
            Self::List(values) => Value::List(values.into_iter().map(Self::into_value).collect()),
            Self::Map(values) => Value::Map(
                values
                    .into_iter()
                    .map(|(key, value)| (key, value.into_value()))
                    .collect(),
            ),
        }
    }
}

fn value_requires_typed_encoding(value: &Value) -> bool {
    if blob_marker_info(value).is_some() {
        return true;
    }
    match value {
        Value::List(items) => {
            items.is_empty()
                || items
                    .iter()
                    .all(|item| matches!(item, Value::Int(value) if u8::try_from(*value).is_ok()))
                || items.iter().any(value_requires_typed_encoding)
        }
        Value::Map(values) => values.values().any(value_requires_typed_encoding),
        Value::Bytes(_) => true,
        _ => false,
    }
}

/// Decode the unversioned document-body representation written before
/// `Value::Bytes` gained an explicit tag. New ambiguous lists and nested byte
/// values are stored as typed blobs, so a byte-range JSON array that still
/// appears inline can only be a legacy body and keeps its historical meaning.
fn decode_legacy_document_body(body: &str) -> SQLiteResult<Document> {
    let serde_json::Value::Object(fields) = serde_json::from_str(body)? else {
        return Err(SQLiteError::StorageBackend(
            "persisted document body is not a JSON object".into(),
        ));
    };
    fields
        .into_iter()
        .map(|(field, value)| Ok((field, decode_legacy_json_value(value)?)))
        .collect()
}

fn decode_legacy_json_value(value: serde_json::Value) -> SQLiteResult<Value> {
    match value {
        serde_json::Value::Null => Ok(Value::Null),
        serde_json::Value::Bool(value) => Ok(Value::Bool(value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(Value::Int(value))
            } else if let Some(value) = value.as_u64() {
                Ok(i64::try_from(value).map_or(Value::Float(value as f64), Value::Int))
            } else if let Some(value) = value.as_f64() {
                Ok(Value::Float(value))
            } else {
                Err(SQLiteError::StorageBackend(
                    "persisted document number is outside the supported numeric range".into(),
                ))
            }
        }
        serde_json::Value::String(value) => Ok(Value::Str(value)),
        serde_json::Value::Array(values) => {
            let mut decoded = Vec::new();
            decoded
                .try_reserve_exact(values.len())
                .map_err(|error| allocation_error("legacy document array", error))?;
            for value in values {
                decoded.push(decode_legacy_json_value(value)?);
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(decoded.len())
                .map_err(|error| allocation_error("legacy document byte array", error))?;
            for value in &decoded {
                let Value::Int(value) = value else {
                    return Ok(Value::List(decoded));
                };
                let Ok(value) = u8::try_from(*value) else {
                    return Ok(Value::List(decoded));
                };
                bytes.push(value);
            }
            Ok(Value::Bytes(bytes))
        }
        serde_json::Value::Object(values) => {
            if values.contains_key("$uqa_type") {
                let tagged = serde_json::Value::Object(values.clone());
                let decoded = serde_json::from_value::<Value>(tagged)?;
                if !matches!(decoded, Value::Map(_)) {
                    return Ok(decoded);
                }
            }
            let mut decoded = BTreeMap::new();
            for (key, value) in values {
                decoded.insert(key, decode_legacy_json_value(value)?);
            }
            Ok(Value::Map(decoded))
        }
    }
}

fn encode_typed_value(field: &str, value: Value) -> SQLiteResult<(Value, Option<Vec<u8>>)> {
    let bytes = serde_json::to_vec(&StoredValue::from_value(value))?;
    Ok((
        value_blob_marker(field.to_string(), VALUE_BLOB_TYPED_JSON),
        Some(bytes),
    ))
}

fn encode_document_blobs(document: Document) -> SQLiteResult<EncodedDocument> {
    let mut stored = Document::new();
    let mut blobs = Vec::new();
    for (field, value) in document {
        let (stored_value, blob) = encode_stored_value(&field, value)?;
        stored.insert(field.clone(), stored_value);
        if let Some(bytes) = blob {
            blobs.push((field, bytes));
        }
    }
    Ok((stored, blobs))
}

fn encode_stored_value(field: &str, value: Value) -> SQLiteResult<(Value, Option<Vec<u8>>)> {
    match value {
        Value::Bytes(bytes) => Ok((blob_marker(field.to_string()), Some(bytes))),
        Value::List(items) => {
            if let Some(bytes) = encode_f64_tensor_blob(&items)? {
                Ok((
                    value_blob_marker(field.to_string(), VALUE_BLOB_F64_TENSOR),
                    Some(bytes),
                ))
            } else if let Some(bytes) = encode_f64_list_blob(&items)? {
                Ok((
                    value_blob_marker(field.to_string(), VALUE_BLOB_F64_LIST),
                    Some(bytes),
                ))
            } else {
                let value = Value::List(items);
                if value_requires_typed_encoding(&value) {
                    encode_typed_value(field, value)
                } else {
                    Ok((value, None))
                }
            }
        }
        other if value_requires_typed_encoding(&other) => encode_typed_value(field, other),
        other => Ok((other, None)),
    }
}

fn encode_f64_list_blob(items: &[Value]) -> SQLiteResult<Option<Vec<u8>>> {
    if items.len() < MIN_NUMERIC_BLOB_VALUES {
        return Ok(None);
    }
    let capacity = items
        .len()
        .checked_mul(std::mem::size_of::<f64>())
        .ok_or_else(|| SQLiteError::StorageBackend("f64-list payload size overflow".into()))?;
    let mut out = Vec::new();
    out.try_reserve_exact(capacity)
        .map_err(|error| allocation_error("f64-list payload", error))?;
    for item in items {
        let Some(value) = value_as_finite_f64(item) else {
            return Ok(None);
        };
        out.extend_from_slice(&value.to_le_bytes());
    }
    Ok(Some(out))
}

fn encode_f64_tensor_blob(items: &[Value]) -> SQLiteResult<Option<Vec<u8>>> {
    let rows = items.len();
    let Some(Value::List(first)) = items.first() else {
        return Ok(None);
    };
    let cols = first.len();
    let Some(value_count) = rows.checked_mul(cols) else {
        return Ok(None);
    };
    if rows == 0 || cols == 0 || value_count < MIN_NUMERIC_BLOB_VALUES {
        return Ok(None);
    }
    let Ok(rows_u32) = u32::try_from(rows) else {
        return Ok(None);
    };
    let Ok(cols_u32) = u32::try_from(cols) else {
        return Ok(None);
    };
    let payload_len = value_count
        .checked_mul(std::mem::size_of::<f64>())
        .ok_or_else(|| SQLiteError::StorageBackend("f64-tensor payload size overflow".into()))?;
    let capacity = 8usize
        .checked_add(payload_len)
        .ok_or_else(|| SQLiteError::StorageBackend("f64-tensor payload size overflow".into()))?;
    let mut out = Vec::new();
    out.try_reserve_exact(capacity)
        .map_err(|error| allocation_error("f64-tensor payload", error))?;
    out.extend_from_slice(&rows_u32.to_le_bytes());
    out.extend_from_slice(&cols_u32.to_le_bytes());
    for row in items {
        let Value::List(values) = row else {
            return Ok(None);
        };
        if values.len() != cols {
            return Ok(None);
        }
        for value in values {
            let Some(value) = value_as_finite_f64(value) else {
                return Ok(None);
            };
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    Ok(Some(out))
}

fn value_as_finite_f64(value: &Value) -> Option<f64> {
    let value = match value {
        Value::Float(value) => *value,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

/// Take `field` out of a parsed document body, hydrating a blob marker
/// into its stored bytes. `Ok(None)` means the document has no such
/// field, so the caller keeps its absent-field default.
fn take_requested_field(
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

fn decode_json_field_value(
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

fn blob_marker(field: String) -> Value {
    Value::Map(BTreeMap::from([
        (
            BLOB_MARKER_TYPE.to_string(),
            Value::Str(BLOB_MARKER_VALUE.to_string()),
        ),
        (BLOB_MARKER_FIELD.to_string(), Value::Str(field)),
    ]))
}

fn value_blob_marker(field: String, encoding: &str) -> Value {
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
enum BlobMarker {
    Bytes(String),
    F64List(String),
    F64Tensor(String),
    TypedValue(String),
}

fn blob_marker_info(value: &Value) -> Option<BlobMarker> {
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

fn hydrate_document_blobs(
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

fn load_marked_document_blob(
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

fn decode_f64_list_blob(bytes: &[u8]) -> SQLiteResult<Option<Value>> {
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

fn decode_f64_tensor_blob(bytes: &[u8]) -> SQLiteResult<Option<Value>> {
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

fn load_document_blob(
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

fn delete_document_blob(
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

fn upsert_document_blob(
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

impl DocumentStore for SQLiteDocumentStore {
    fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
        self.put_inner(doc_id, &document)?;
        Ok(())
    }

    fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
        Ok(self.get_inner(doc_id)?)
    }

    fn contains_doc_id(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        let sqlite_doc_id = sqlite_doc_id(doc_id)?;
        Ok(self.conn.with(|c| {
            let found: Option<i64> = c
                .prepare_cached(
                    "SELECT 1 FROM _documents
                         WHERE table_name = ?1 AND doc_id = ?2
                         LIMIT 1",
                )?
                .query_row(params![self.table, sqlite_doc_id], |r| r.get(0))
                .optional()?;
            Ok(found.is_some())
        })?)
    }

    fn get_field(
        &self,
        doc_id: DocId,
        field: &str,
    ) -> StorageBackendResult<Option<uqa_core::Value>> {
        Ok(self.get_field_inner(doc_id, field)?)
    }

    fn find_doc_id_by_field(
        &self,
        field: &str,
        value: &Value,
    ) -> StorageBackendResult<Option<DocId>> {
        Ok(self.find_doc_id_by_field_inner(field, value)?)
    }

    fn get_fields_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, Value>> {
        let mut out: BTreeMap<DocId, Value> = doc_ids
            .iter()
            .copied()
            .map(|doc_id| (doc_id, Value::Null))
            .collect();
        if doc_ids.is_empty() {
            return Ok(out);
        }
        // Fetch the document body and extract the field in Rust: one
        // JSON parse per row. Extracting through `json_type` +
        // `json_extract` made `SQLite` parse the same body twice per
        // requested field.
        let mut decode_row = |c: &rusqlite::Connection,
                              row: &rusqlite::Row<'_>|
         -> SQLiteResult<()> {
            let doc_id = read_doc_id(row, 0)?;
            let body = row.get::<_, String>(1)?;
            let mut document = decode_legacy_document_body(&body)?;
            if let Some(value) = take_requested_field(c, &self.table, doc_id, &mut document, field)?
            {
                out.insert(doc_id, value);
            }
            Ok(())
        };

        // Selective requests probe by id; wide requests (half the
        // table or more) sequential-scan once instead of issuing many
        // B-tree probes.
        let should_probe =
            doc_ids.len() <= DOC_ID_IN_CHUNK || should_probe_doc_ids(doc_ids.len(), self.len()?);
        if should_probe {
            let leading = [rusqlite::types::Value::Text(self.table.clone())];
            let sql = format!(
                "SELECT doc_id, body FROM _documents
                 WHERE table_name = ?1 AND doc_id IN ({})",
                doc_id_in_placeholders(2, DOC_ID_IN_CHUNK)?
            );
            self.conn.with(|c| {
                for chunk in doc_ids.chunks(DOC_ID_IN_CHUNK) {
                    let mut stmt = c.prepare_cached(&sql)?;
                    let bind = chunk_bind_values(&leading, chunk)?;
                    let mut rows = stmt.query(rusqlite::params_from_iter(bind))?;
                    while let Some(row) = rows.next()? {
                        decode_row(c, row)?;
                    }
                }
                Ok(())
            })?;
            return Ok(out);
        }

        let requested = sorted_unique_doc_ids(doc_ids)?;
        self.conn.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT doc_id, body FROM _documents
                 WHERE table_name = ?1
                 ORDER BY doc_id",
            )?;
            let mut rows = stmt.query(params![self.table])?;
            while let Some(row) = rows.next()? {
                let doc_id = read_doc_id(row, 0)?;
                if requested.binary_search(&doc_id).is_err() {
                    continue;
                }
                decode_row(c, row)?;
            }
            Ok(())
        })?;
        Ok(out)
    }

    fn get_fields_multi(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        let mut out: BTreeMap<DocId, Vec<Value>> = BTreeMap::new();
        if doc_ids.is_empty() || fields.is_empty() {
            return Ok(out);
        }
        // Fetch the document body and extract every requested field in
        // Rust: one JSON parse per row, however many fields the caller
        // asked for. The previous `json_type` + `json_extract` pair per
        // field made `SQLite` parse the same body twice per field.
        let decode_row = |c: &rusqlite::Connection,
                          row: &rusqlite::Row<'_>|
         -> SQLiteResult<(DocId, Vec<Value>)> {
            let doc_id = read_doc_id(row, 0)?;
            let body = row.get::<_, String>(1)?;
            let document = decode_legacy_document_body(&body)?;
            let mut values = Vec::new();
            values
                .try_reserve_exact(fields.len())
                .map_err(|error| allocation_error("multi-field document values", error))?;
            for field in fields {
                let mut value = document.get(*field).cloned().unwrap_or(Value::Null);
                if let Some(marker) = blob_marker_info(&value) {
                    if let Some(decoded) =
                        load_marked_document_blob(c, &self.table, doc_id, field, &marker)?
                    {
                        value = decoded;
                    }
                }
                values.push(value);
            }
            Ok((doc_id, values))
        };

        let should_probe =
            doc_ids.len() <= DOC_ID_IN_CHUNK || should_probe_doc_ids(doc_ids.len(), self.len()?);
        if should_probe {
            let leading = [rusqlite::types::Value::Text(self.table.clone())];
            let sql = format!(
                "SELECT doc_id, body FROM _documents
                 WHERE table_name = ?1 AND doc_id IN ({})",
                doc_id_in_placeholders(2, DOC_ID_IN_CHUNK)?
            );
            self.conn.with(|c| {
                for chunk in doc_ids.chunks(DOC_ID_IN_CHUNK) {
                    let mut stmt = c.prepare_cached(&sql)?;
                    let bind = chunk_bind_values(&leading, chunk)?;
                    let mut rows = stmt.query(rusqlite::params_from_iter(bind))?;
                    while let Some(row) = rows.next()? {
                        let (doc_id, values) = decode_row(c, row)?;
                        out.insert(doc_id, values);
                    }
                }
                Ok(())
            })?;
            return Ok(out);
        }

        let requested = sorted_unique_doc_ids(doc_ids)?;
        self.conn.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT doc_id, body FROM _documents
                 WHERE table_name = ?1
                 ORDER BY doc_id",
            )?;
            let mut rows = stmt.query(params![self.table])?;
            while let Some(row) = rows.next()? {
                let doc_id = read_doc_id(row, 0)?;
                if requested.binary_search(&doc_id).is_err() {
                    continue;
                }
                let (doc_id, values) = decode_row(c, row)?;
                out.insert(doc_id, values);
            }
            Ok(())
        })?;
        Ok(out)
    }

    fn get_many(&self, doc_ids: &[DocId]) -> StorageBackendResult<BTreeMap<DocId, Document>> {
        let mut out: BTreeMap<DocId, Document> = BTreeMap::new();
        if doc_ids.is_empty() {
            return Ok(out);
        }
        // Same probe-vs-scan split as `get_fields_bulk`.
        let should_probe =
            doc_ids.len() <= DOC_ID_IN_CHUNK || should_probe_doc_ids(doc_ids.len(), self.len()?);
        if should_probe {
            let leading = [rusqlite::types::Value::Text(self.table.clone())];
            let sql = format!(
                "SELECT doc_id, body FROM _documents
                 WHERE table_name = ?1 AND doc_id IN ({})",
                doc_id_in_placeholders(2, DOC_ID_IN_CHUNK)?
            );
            self.conn.with(|c| {
                for chunk in doc_ids.chunks(DOC_ID_IN_CHUNK) {
                    let mut stmt = c.prepare_cached(&sql)?;
                    let bind = chunk_bind_values(&leading, chunk)?;
                    let mut rows = stmt.query(rusqlite::params_from_iter(bind))?;
                    while let Some(row) = rows.next()? {
                        let doc_id = read_doc_id(row, 0)?;
                        let body = row.get::<_, String>(1)?;
                        let mut document = decode_legacy_document_body(&body)?;
                        hydrate_document_blobs(c, &self.table, doc_id, &mut document)?;
                        out.insert(doc_id, document);
                    }
                }
                Ok(())
            })?;
            return Ok(out);
        }

        let requested = sorted_unique_doc_ids(doc_ids)?;
        self.conn.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT doc_id, body FROM _documents
                 WHERE table_name = ?1
                 ORDER BY doc_id",
            )?;
            let mut rows = stmt.query(params![self.table])?;
            while let Some(row) = rows.next()? {
                let doc_id = read_doc_id(row, 0)?;
                if requested.binary_search(&doc_id).is_err() {
                    continue;
                }
                let body = row.get::<_, String>(1)?;
                let mut document = decode_legacy_document_body(&body)?;
                hydrate_document_blobs(c, &self.table, doc_id, &mut document)?;
                out.insert(doc_id, document);
            }
            Ok(())
        })?;
        Ok(out)
    }

    fn patch_fields(
        &mut self,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
    ) -> StorageBackendResult<bool> {
        Ok(self.patch_fields_inner(doc_id, updates)?)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let sqlite_doc_id = sqlite_doc_id(doc_id)?;
        self.conn.with(|c| {
            c.prepare_cached(&format!(
                "DELETE FROM {DOCUMENT_BLOBS_TABLE}
                 WHERE table_name = ?1 AND doc_id = ?2"
            ))?
            .execute(params![self.table, sqlite_doc_id])?;
            c.prepare_cached("DELETE FROM _documents WHERE table_name = ?1 AND doc_id = ?2")?
                .execute(params![self.table, sqlite_doc_id])?;
            Ok(())
        })?;
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.conn.with(|c| {
            c.execute(
                &format!("DELETE FROM {DOCUMENT_BLOBS_TABLE} WHERE table_name = ?1"),
                params![self.table],
            )?;
            c.execute(
                "DELETE FROM _documents WHERE table_name = ?1",
                params![self.table],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
        Ok(self.conn.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT doc_id FROM _documents WHERE table_name = ?1 ORDER BY doc_id",
            )?;
            let rows = stmt.query_map(params![self.table], |r| r.get::<_, i64>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(document_id_from_sqlite(row?)?);
            }
            Ok(out)
        })?)
    }

    fn next_doc_id(&self, after: Option<DocId>) -> StorageBackendResult<Option<DocId>> {
        let after = after.map(sqlite_doc_id).transpose()?;
        Ok(self.conn.with(|connection| {
            let doc_id: Option<i64> = match after {
                Some(after) => connection
                    .prepare_cached(
                        "SELECT doc_id FROM _documents
                         WHERE table_name = ?1 AND doc_id > ?2
                         ORDER BY doc_id LIMIT 1",
                    )?
                    .query_row(params![self.table, after], |row| row.get::<_, i64>(0))
                    .optional()?,
                None => connection
                    .prepare_cached(
                        "SELECT doc_id FROM _documents
                         WHERE table_name = ?1
                         ORDER BY doc_id LIMIT 1",
                    )?
                    .query_row(params![self.table], |row| row.get::<_, i64>(0))
                    .optional()?,
            };
            doc_id.map(document_id_from_sqlite).transpose()
        })?)
    }

    fn max_doc_id(&self) -> StorageBackendResult<DocId> {
        SQLiteDocumentStore::max_doc_id(self)
    }

    fn len(&self) -> StorageBackendResult<usize> {
        Ok(self.conn.with(|c| {
            let n: i64 = c
                .prepare_cached("SELECT COUNT(*) FROM _documents WHERE table_name = ?1")?
                .query_row(params![self.table], |r| r.get(0))?;
            usize::try_from(n).map_err(|_| {
                SQLiteError::StorageBackend(format!(
                    "document count {n} is outside the addressable range"
                ))
            })
        })?)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
        Ok(Arc::new(self.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::catalog::Catalog;
    use crate::StorageBackendError;
    use uqa_core::Value;

    fn store() -> SQLiteDocumentStore {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _cat = Catalog::open(mc.clone()).unwrap();
        SQLiteDocumentStore::new(mc, "articles")
    }

    fn doc<const N: usize>(pairs: [(&str, Value); N]) -> Document {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn put_get_round_trip() {
        let mut s = store();
        s.put(1, doc([("title", Value::Str("rust".into()))]))
            .unwrap();
        let got = s.get(1).unwrap().unwrap();
        assert_eq!(got.get("title"), Some(&Value::Str("rust".into())));
    }

    #[test]
    fn typed_lists_round_trip_without_becoming_bytes_or_floats() {
        let mut s = store();
        let expected = doc([
            (
                "short_ints",
                Value::List(vec![Value::Int(10), Value::Int(20)]),
            ),
            ("empty", Value::List(Vec::new())),
            (
                "matrix",
                Value::List(vec![
                    Value::List(vec![Value::Int(1), Value::Int(2)]),
                    Value::List(vec![Value::Int(3), Value::Int(4)]),
                ]),
            ),
            (
                "nested_map",
                Value::Map(BTreeMap::from([(
                    "flags".into(),
                    Value::List(vec![Value::Int(0), Value::Int(1)]),
                )])),
            ),
            ("long_ints", Value::List((0..64).map(Value::Int).collect())),
            ("bytes", Value::Bytes(vec![1, 2, 3])),
        ]);
        s.put(2, expected.clone()).unwrap();

        let restored = s.get(2).unwrap().unwrap();
        assert_eq!(restored, expected);
        assert_eq!(
            s.get_field(2, "short_ints").unwrap(),
            Some(Value::List(vec![Value::Int(10), Value::Int(20)]))
        );
        assert_eq!(
            s.get_fields_bulk(&[2], "empty").unwrap().get(&2),
            Some(&Value::List(Vec::new()))
        );
        assert_eq!(s.get_many(&[2]).unwrap().get(&2), Some(&expected));

        s.conn
            .with(|connection| {
                let body: String = connection.query_row(
                    "SELECT body FROM _documents
                     WHERE table_name = 'articles' AND doc_id = 2",
                    [],
                    |row| row.get(0),
                )?;
                assert!(body.contains(VALUE_BLOB_TYPED_JSON), "{body}");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn user_maps_that_resemble_internal_blob_markers_round_trip_as_data() {
        let mut s = store();
        let expected = doc([
            ("bytes_marker", blob_marker("other_field".into())),
            (
                "value_marker",
                value_blob_marker("other_field".into(), VALUE_BLOB_F64_LIST),
            ),
        ]);
        s.put(4, expected.clone()).unwrap();

        assert_eq!(s.get(4).unwrap(), Some(expected.clone()));
        assert_eq!(
            s.get_field(4, "bytes_marker").unwrap(),
            expected.get("bytes_marker").cloned()
        );
        assert_eq!(
            s.get_fields_multi(&[4], &["bytes_marker", "value_marker"])
                .unwrap()
                .get(&4),
            Some(&vec![
                expected["bytes_marker"].clone(),
                expected["value_marker"].clone(),
            ])
        );
    }

    #[test]
    fn patch_fields_preserves_ambiguous_list_variants() {
        let mut s = store();
        s.put(3, doc([("value", Value::Str("old".into()))]))
            .unwrap();
        let updates = BTreeMap::from([(
            "value".to_string(),
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        )]);
        assert!(s.patch_fields(3, &updates).unwrap());
        assert_eq!(
            s.get_field(3, "value").unwrap(),
            Some(Value::List(vec![Value::Int(1), Value::Int(2)]))
        );
    }

    #[test]
    fn placeholder_builder_reports_size_and_index_overflow() {
        assert!(doc_id_in_placeholders(1, usize::MAX).is_err());
        assert!(doc_id_in_placeholders(usize::MAX, 2).is_err());
    }

    #[test]
    fn document_id_larger_than_sqlite_integer_is_rejected() {
        let mut s = store();
        let error = s.put(DocId::MAX, Document::new()).unwrap_err();
        assert!(matches!(
            error,
            StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message))
                if message.contains("exceeds SQLite INTEGER")
        ));
        assert_eq!(s.len().unwrap(), 0, "failed write must not insert a row");

        assert!(matches!(
            s.get(DocId::MAX),
            Err(StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message)))
                if message.contains("exceeds SQLite INTEGER")
        ));
        assert!(matches!(
            s.get_many(&[DocId::MAX]),
            Err(StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message)))
                if message.contains("exceeds SQLite INTEGER")
        ));
    }

    #[test]
    fn negative_persisted_document_id_is_reported_as_corruption() {
        let s = store();
        s.conn
            .with(|connection| {
                connection.execute(
                    "INSERT INTO _documents (table_name, doc_id, body) VALUES (?1, ?2, ?3)",
                    params!["articles", -1_i64, "{}"],
                )?;
                Ok(())
            })
            .unwrap();

        assert!(matches!(
            s.doc_ids(),
            Err(StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message)))
                if message.contains("negative SQLite document id -1")
        ));
        assert!(matches!(
            s.next_doc_id(None),
            Err(StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message)))
                if message.contains("negative SQLite document id -1")
        ));
        assert!(matches!(
            s.max_doc_id(),
            Err(StorageBackendError::SQLite(SQLiteError::StorageBackend(ref message)))
                if message.contains("negative SQLite document id -1")
        ));
    }

    /// A real storage write failure (`SQLITE_FULL` via `max_page_count`)
    /// must surface as `Err`, never as a silently-dropped write: callers
    /// use this signal to abort the enclosing statement or transaction.
    #[test]
    fn put_failure_is_reported_not_swallowed() {
        let mut s = store();
        s.put(1, doc([("title", Value::Str("small".into()))]))
            .unwrap();
        s.conn
            .with(|c| {
                let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0))?;
                c.pragma_update(None, "max_page_count", pages)?;
                Ok(())
            })
            .unwrap();
        let huge = "x".repeat(8 * 1024 * 1024);
        let err = s.put(2, doc([("body", Value::Str(huge))]));
        assert!(
            err.is_err(),
            "oversized put must fail once the page budget is exhausted"
        );
        // The failure must not have corrupted existing data.
        let got = s.get(1).unwrap().unwrap();
        assert_eq!(got.get("title"), Some(&Value::Str("small".into())));
    }

    #[test]
    fn delete_removes_row() {
        let mut s = store();
        s.put(1, doc([("a", Value::Int(1))])).unwrap();
        s.delete(1).unwrap();
        assert!(s.get(1).unwrap().is_none());
        assert_eq!(s.len().unwrap(), 0);
    }

    #[test]
    fn doc_ids_sorted_ascending() {
        let mut s = store();
        s.put(3, Document::new()).unwrap();
        s.put(1, Document::new()).unwrap();
        s.put(2, Document::new()).unwrap();
        assert_eq!(s.doc_ids().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn get_field_reads_individual_field() {
        let mut s = store();
        s.put(
            7,
            doc([("year", Value::Int(2026)), ("flag", Value::Bool(true))]),
        )
        .unwrap();
        assert_eq!(s.get_field(7, "year").unwrap(), Some(Value::Int(2026)));
        assert_eq!(s.get_field(7, "flag").unwrap(), Some(Value::Bool(true)));
        assert_eq!(s.get_field(7, "missing").unwrap(), None);
    }

    #[test]
    fn get_fields_bulk_reads_values_and_missing_as_null() {
        let mut s = store();
        s.put(
            1,
            doc([
                ("title", Value::Str("rust".into())),
                ("payload", Value::Bytes(vec![1, 2, 3])),
            ]),
        )
        .unwrap();
        s.put(2, doc([("title", Value::Str("sqlite".into()))]))
            .unwrap();

        let titles = s.get_fields_bulk(&[1, 2, 99], "title").unwrap();
        assert_eq!(titles.get(&1), Some(&Value::Str("rust".into())));
        assert_eq!(titles.get(&2), Some(&Value::Str("sqlite".into())));
        assert_eq!(titles.get(&99), Some(&Value::Null));

        let payloads = s.get_fields_bulk(&[1, 2], "payload").unwrap();
        assert_eq!(payloads.get(&1), Some(&Value::Bytes(vec![1, 2, 3])));
        assert_eq!(payloads.get(&2), Some(&Value::Null));
    }

    #[test]
    fn find_doc_id_by_field_uses_top_level_value() {
        let mut s = store();
        s.put(
            5,
            doc([
                ("public_id", Value::Str("m-5".into())),
                ("content", Value::Str("old".into())),
            ]),
        )
        .unwrap();
        s.put(
            9,
            doc([
                ("public_id", Value::Str("m-9".into())),
                ("content", Value::Str("target".into())),
            ]),
        )
        .unwrap();

        assert_eq!(
            s.find_doc_id_by_field("public_id", &Value::Str("m-9".into()))
                .unwrap(),
            Some(9)
        );
        assert_eq!(
            s.find_doc_id_by_field("public_id", &Value::Str("missing".into()))
                .unwrap(),
            None
        );
    }

    #[test]
    fn patch_fields_updates_body_without_losing_unmodified_values() {
        let mut s = store();
        s.put(
            31,
            doc([
                ("public_id", Value::Str("m-31".into())),
                ("content", Value::Str("old".into())),
                (
                    "embedding",
                    Value::List(vec![Value::Float(0.25), Value::Float(0.75)]),
                ),
                ("token_count", Value::Int(2)),
            ]),
        )
        .unwrap();

        let updates = BTreeMap::from([
            ("content".to_string(), Value::Str("new".into())),
            ("token_count".to_string(), Value::Null),
        ]);
        assert!(s.patch_fields(31, &updates).unwrap());

        let got = s.get(31).unwrap().unwrap();
        assert_eq!(got.get("public_id"), Some(&Value::Str("m-31".into())));
        assert_eq!(got.get("content"), Some(&Value::Str("new".into())));
        assert_eq!(
            got.get("embedding"),
            Some(&Value::List(vec![Value::Float(0.25), Value::Float(0.75)]))
        );
        assert!(!got.contains_key("token_count"));
    }

    #[test]
    fn patch_fields_updates_blob_storage() {
        let mut s = store();
        s.put(
            41,
            doc([
                ("public_id", Value::Str("m-41".into())),
                ("bytes", Value::Bytes(vec![1, 2, 3])),
            ]),
        )
        .unwrap();

        let updates = BTreeMap::from([("bytes".to_string(), Value::Bytes(vec![4, 5]))]);
        assert!(s.patch_fields(41, &updates).unwrap());
        assert_eq!(
            s.get_field(41, "bytes").unwrap(),
            Some(Value::Bytes(vec![4, 5]))
        );

        s.conn
            .with(|c| {
                let bytes: Vec<u8> = c.query_row(
                    &format!(
                        "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
                         WHERE table_name = 'articles'
                           AND doc_id = 41
                           AND field_name = 'bytes'"
                    ),
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(bytes, vec![4, 5]);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn byte_values_are_stored_as_sqlite_blobs_not_json_arrays() {
        let mut s = store();
        s.put(
            11,
            doc([
                ("bytes", Value::Bytes(vec![1, 2, 3, 4])),
                ("title", Value::Str("asset".into())),
            ]),
        )
        .unwrap();

        s.conn
            .with(|c| {
                let body: String = c.query_row(
                    "SELECT body FROM _documents
                     WHERE table_name = 'articles' AND doc_id = 11",
                    [],
                    |r| r.get(0),
                )?;
                assert!(body.contains(BLOB_MARKER_VALUE), "{body}");
                assert!(!body.contains("\"bytes\":[1,2,3,4]"), "{body}");

                let bytes: Vec<u8> = c.query_row(
                    &format!(
                        "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
                         WHERE table_name = 'articles'
                           AND doc_id = 11
                           AND field_name = 'bytes'"
                    ),
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(bytes, vec![1, 2, 3, 4]);
                Ok(())
            })
            .unwrap();

        assert_eq!(
            s.get_field(11, "bytes").unwrap(),
            Some(Value::Bytes(vec![1, 2, 3, 4]))
        );
        let got = s.get(11).unwrap().unwrap();
        assert_eq!(got.get("title"), Some(&Value::Str("asset".into())));
    }

    #[test]
    fn large_numeric_values_are_stored_as_sqlite_blobs_not_json_arrays() {
        let mut s = store();
        let embedding = Value::List((0..64).map(|i| Value::Float(f64::from(i) / 64.0)).collect());
        let tensor = Value::List(
            (0..4)
                .map(|row| {
                    Value::List(
                        (0..16)
                            .map(|col| Value::Float(f64::from(row * 16 + col)))
                            .collect(),
                    )
                })
                .collect(),
        );

        s.put(
            12,
            doc([
                ("embedding", embedding.clone()),
                ("tensor", tensor.clone()),
                ("title", Value::Str("vector".into())),
            ]),
        )
        .unwrap();

        s.conn
            .with(|c| {
                let body: String = c.query_row(
                    "SELECT body FROM _documents
                     WHERE table_name = 'articles' AND doc_id = 12",
                    [],
                    |r| r.get(0),
                )?;
                assert!(body.contains(VALUE_BLOB_MARKER_VALUE), "{body}");
                assert!(body.contains(VALUE_BLOB_F64_LIST), "{body}");
                assert!(body.contains(VALUE_BLOB_F64_TENSOR), "{body}");
                assert!(!body.contains("0.984375"), "{body}");
                assert!(!body.contains("63.0"), "{body}");

                let embedding_bytes: Vec<u8> = c.query_row(
                    &format!(
                        "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
                         WHERE table_name = 'articles'
                           AND doc_id = 12
                           AND field_name = 'embedding'"
                    ),
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(embedding_bytes.len(), 64 * std::mem::size_of::<f64>());

                let tensor_bytes: Vec<u8> = c.query_row(
                    &format!(
                        "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
                         WHERE table_name = 'articles'
                           AND doc_id = 12
                           AND field_name = 'tensor'"
                    ),
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(tensor_bytes.len(), 8 + 64 * std::mem::size_of::<f64>());
                Ok(())
            })
            .unwrap();

        assert_eq!(
            s.get_field(12, "embedding").unwrap(),
            Some(embedding.clone())
        );
        assert_eq!(s.get_field(12, "tensor").unwrap(), Some(tensor.clone()));

        let fields = s.get_fields_bulk(&[12, 99], "embedding").unwrap();
        assert_eq!(fields.get(&12), Some(&embedding));
        assert_eq!(fields.get(&99), Some(&Value::Null));

        let got = s.get(12).unwrap().unwrap();
        assert_eq!(got.get("embedding"), Some(&embedding));
        assert_eq!(got.get("tensor"), Some(&tensor));
        assert_eq!(got.get("title"), Some(&Value::Str("vector".into())));
    }

    #[test]
    fn legacy_inline_byte_arrays_are_read_without_hidden_writes() {
        let s = store();
        s.conn
            .with(|c| {
                c.execute(
                    "INSERT INTO _documents (table_name, doc_id, body)
                     VALUES ('articles', 21, ?1)",
                    [r#"{"bytes":[9,8,7],"title":"legacy"}"#],
                )?;
                Ok(())
            })
            .unwrap();

        let got = s.get(21).unwrap().unwrap();
        assert_eq!(got.get("bytes"), Some(&Value::Bytes(vec![9, 8, 7])));
        assert_eq!(got.get("title"), Some(&Value::Str("legacy".into())));

        s.conn
            .with(|c| {
                let body: String = c.query_row(
                    "SELECT body FROM _documents
                     WHERE table_name = 'articles' AND doc_id = 21",
                    [],
                    |r| r.get(0),
                )?;
                assert!(!body.contains(BLOB_MARKER_VALUE), "{body}");
                assert!(body.contains("\"bytes\":[9,8,7]"), "{body}");

                let blob_rows: i64 = c.query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {DOCUMENT_BLOBS_TABLE}
                         WHERE table_name = 'articles'
                           AND doc_id = 21
                           AND field_name = 'bytes'"
                    ),
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(blob_rows, 0);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn missing_or_malformed_blob_rows_are_reported_as_corruption() {
        let mut s = store();
        s.put(31, doc([("bytes", Value::Bytes(vec![1, 2, 3]))]))
            .unwrap();
        s.conn
            .with(|c| {
                c.execute(
                    "DELETE FROM _document_blobs
                     WHERE table_name = 'articles' AND doc_id = 31",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            s.get(31),
            Err(StorageBackendError::SQLite(
                SQLiteError::CorruptDocumentBlob { .. }
            ))
        ));

        let embedding = Value::List(
            (0..64)
                .map(|value| Value::Float(f64::from(value)))
                .collect(),
        );
        s.put(32, doc([("embedding", embedding)])).unwrap();
        s.conn
            .with(|c| {
                c.execute(
                    "UPDATE _document_blobs SET bytes = x'00'
                     WHERE table_name = 'articles' AND doc_id = 32",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            s.get_field(32, "embedding"),
            Err(StorageBackendError::SQLite(
                SQLiteError::CorruptDocumentBlob { .. }
            ))
        ));
    }

    #[test]
    fn tensor_decoder_rejects_dimension_product_overflow() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode_f64_tensor_blob(&bytes).unwrap(), None);
    }

    #[test]
    fn delete_and_clear_remove_blob_rows() {
        let mut s = store();
        s.put(1, doc([("bytes", Value::Bytes(vec![1]))])).unwrap();
        s.put(2, doc([("bytes", Value::Bytes(vec![2]))])).unwrap();

        s.delete(1).unwrap();
        let remaining = s
            .conn
            .with(|c| {
                Ok(c.query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {DOCUMENT_BLOBS_TABLE}
                         WHERE table_name = 'articles'"
                    ),
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap();
        assert_eq!(remaining, 1);

        s.clear().unwrap();
        let remaining = s
            .conn
            .with(|c| {
                Ok(c.query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {DOCUMENT_BLOBS_TABLE}
                         WHERE table_name = 'articles'"
                    ),
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap();
        assert_eq!(remaining, 0);
    }
}
