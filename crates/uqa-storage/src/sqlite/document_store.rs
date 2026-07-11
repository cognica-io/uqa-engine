//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed [`DocumentStore`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use uqa_core::{DocId, Value};

use crate::backend::StorageBackendResult;
use crate::document_store::{Document, DocumentStore};
use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult};

const DOCUMENT_BLOBS_TABLE: &str = "_document_blobs";
const BLOB_MARKER_TYPE: &str = "$uqa_type";
const BLOB_MARKER_VALUE: &str = "document_blob";
const BLOB_MARKER_FIELD: &str = "field";
const BLOB_MARKER_ENCODING: &str = "encoding";
const VALUE_BLOB_MARKER_VALUE: &str = "value_blob";
const VALUE_BLOB_F64_LIST: &str = "f64_list";
const VALUE_BLOB_F64_TENSOR: &str = "f64_tensor";
const MIN_NUMERIC_BLOB_VALUES: usize = 32;

/// Batch size for `doc_id IN (...)` reads. Partial batches are padded
/// by repeating the final id so every batch reuses one cached
/// statement text.
const DOC_ID_IN_CHUNK: usize = 256;

/// Build `?first,?first+1,...` placeholders for a doc-id IN clause.
fn doc_id_in_placeholders(first_index: usize, count: usize) -> String {
    let mut out = String::with_capacity(count * 5);
    for i in 0..count {
        if i > 0 {
            out.push(',');
        }
        out.push('?');
        out.push_str(&(first_index + i).to_string());
    }
    out
}

/// Bind values for one padded id batch: leading params (table name,
/// optional JSON path) followed by exactly `DOC_ID_IN_CHUNK` ids.
fn chunk_bind_values(
    leading: &[rusqlite::types::Value],
    chunk: &[DocId],
) -> Vec<rusqlite::types::Value> {
    let mut bind = Vec::with_capacity(leading.len() + DOC_ID_IN_CHUNK);
    bind.extend_from_slice(leading);
    let pad = chunk.last().copied().unwrap_or(0);
    for i in 0..DOC_ID_IN_CHUNK {
        let id = chunk.get(i).copied().unwrap_or(pad);
        bind.push(rusqlite::types::Value::Integer(id as i64));
    }
    bind
}

#[derive(Clone)]
pub struct SQLiteDocumentStore {
    conn: ManagedConnection,
    table: String,
}

impl SQLiteDocumentStore {
    pub fn new(conn: ManagedConnection, table: impl Into<String>) -> Self {
        let store = Self {
            conn,
            table: table.into(),
        };
        let _ = store.ensure_blob_table();
        store
    }

    pub fn max_doc_id(&self) -> DocId {
        self.conn
            .with(|c| {
                let id: Option<i64> = c
                    .prepare_cached("SELECT MAX(doc_id) FROM _documents WHERE table_name = ?1")?
                    .query_row(params![self.table], |r| r.get(0))?;
                Ok(id.unwrap_or(0) as DocId)
            })
            .unwrap_or(0)
    }

    fn put_inner(&self, doc_id: DocId, document: &Document) -> SQLiteResult<()> {
        let document: Document = document
            .iter()
            .filter(|(_, value)| !matches!(value, uqa_core::Value::Null))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let (document, blobs) = encode_document_blobs(document);
        let body = serde_json::to_string(&document)?;
        self.conn.with(|c| {
            c.prepare_cached(&format!(
                "DELETE FROM {DOCUMENT_BLOBS_TABLE}
                 WHERE table_name = ?1 AND doc_id = ?2"
            ))?
            .execute(params![self.table, doc_id as i64])?;
            c.prepare_cached(
                "INSERT OR REPLACE INTO _documents (table_name, doc_id, body)
                 VALUES (?1, ?2, ?3)",
            )?
            .execute(params![self.table, doc_id as i64, body])?;
            for (field, bytes) in blobs {
                c.prepare_cached(&format!(
                    "INSERT OR REPLACE INTO {DOCUMENT_BLOBS_TABLE}
                     (table_name, doc_id, field_name, bytes)
                     VALUES (?1, ?2, ?3, ?4)"
                ))?
                .execute(params![self.table, doc_id as i64, field, bytes])?;
            }
            Ok(())
        })
    }

    fn get_inner(&self, doc_id: DocId) -> SQLiteResult<Option<Document>> {
        self.conn.with(|c| {
            let body: Option<String> = c
                .prepare_cached(
                    "SELECT body FROM _documents
                     WHERE table_name = ?1 AND doc_id = ?2",
                )?
                .query_row(params![self.table, doc_id as i64], |r| r.get(0))
                .optional()?;
            let Some(body) = body else {
                return Ok(None);
            };
            let mut document: Document = serde_json::from_str(&body)?;
            let has_inline_bytes = document
                .values()
                .any(|value| matches!(value, Value::Bytes(_)));
            hydrate_document_blobs(c, &self.table, doc_id, &mut document)?;
            if has_inline_bytes {
                let (stored, blobs) = encode_document_blobs(document.clone());
                c.execute(
                    "UPDATE _documents SET body = ?3
                     WHERE table_name = ?1 AND doc_id = ?2",
                    params![self.table, doc_id as i64, serde_json::to_string(&stored)?],
                )?;
                c.execute(
                    &format!(
                        "DELETE FROM {DOCUMENT_BLOBS_TABLE}
                         WHERE table_name = ?1 AND doc_id = ?2"
                    ),
                    params![self.table, doc_id as i64],
                )?;
                for (field, bytes) in blobs {
                    c.execute(
                        &format!(
                            "INSERT OR REPLACE INTO {DOCUMENT_BLOBS_TABLE}
                             (table_name, doc_id, field_name, bytes)
                             VALUES (?1, ?2, ?3, ?4)"
                        ),
                        params![self.table, doc_id as i64, field, bytes],
                    )?;
                }
            }
            Ok(Some(document))
        })
    }

    fn get_field_inner(&self, doc_id: DocId, field: &str) -> SQLiteResult<Option<Value>> {
        let path = sqlite_json_path(field);
        self.conn.with(|c| {
            let row: Option<(Option<String>, String)> = c
                .prepare_cached(
                    "SELECT json_type(body, ?3), json_quote(json_extract(body, ?3))
                     FROM _documents
                     WHERE table_name = ?1 AND doc_id = ?2",
                )?
                .query_row(params![self.table, doc_id as i64, path], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?;
            let Some((json_type, json_text)) = row else {
                return Ok(None);
            };
            decode_json_field_value(c, &self.table, doc_id, json_type, &json_text)
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
                Ok(doc_id.map(|id| id as DocId))
            }),
            _ => Ok(self
                .doc_ids()
                .into_iter()
                .find(|id| self.get_field(*id, field).as_ref() == Some(value))),
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
        self.conn.with(|c| {
            let exists: Option<i64> = c
                .prepare_cached(
                    "SELECT 1 FROM _documents
                     WHERE table_name = ?1 AND doc_id = ?2
                     LIMIT 1",
                )?
                .query_row(params![self.table, doc_id as i64], |r| r.get(0))
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
                            params![self.table, doc_id as i64, path],
                        )?;
                    }
                    Value::Bytes(bytes) => {
                        let marker = serde_json::to_string(&blob_marker(field.clone()))?;
                        c.execute(
                            "UPDATE _documents SET body = json_set(body, ?3, json(?4))
                             WHERE table_name = ?1 AND doc_id = ?2",
                            params![self.table, doc_id as i64, path, marker],
                        )?;
                        upsert_document_blob(c, &self.table, doc_id, field, bytes)?;
                    }
                    other => {
                        let (stored, blob) = encode_stored_value(field, other.clone());
                        let json = serde_json::to_string(&stored)?;
                        c.execute(
                            "UPDATE _documents SET body = json_set(body, ?3, json(?4))
                             WHERE table_name = ?1 AND doc_id = ?2",
                            params![self.table, doc_id as i64, path, json],
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

    fn ensure_blob_table(&self) -> SQLiteResult<()> {
        self.conn.with(|c| {
            c.execute(
                &format!(
                    "CREATE TABLE IF NOT EXISTS {DOCUMENT_BLOBS_TABLE} (
                       table_name TEXT NOT NULL,
                       doc_id INTEGER NOT NULL,
                       field_name TEXT NOT NULL,
                       bytes BLOB NOT NULL,
                       PRIMARY KEY (table_name, doc_id, field_name)
                     )"
                ),
                [],
            )?;
            Ok(())
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
        format!("$.{}", serde_json::to_string(field).unwrap_or_default())
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
    Ok(doc_id.map(|id| id as DocId))
}

fn encode_document_blobs(document: Document) -> (Document, Vec<(String, Vec<u8>)>) {
    let mut stored = Document::new();
    let mut blobs = Vec::new();
    for (field, value) in document {
        let (stored_value, blob) = encode_stored_value(&field, value);
        stored.insert(field.clone(), stored_value);
        if let Some(bytes) = blob {
            blobs.push((field, bytes));
        }
    }
    (stored, blobs)
}

fn encode_stored_value(field: &str, value: Value) -> (Value, Option<Vec<u8>>) {
    match value {
        Value::Bytes(bytes) => (blob_marker(field.to_string()), Some(bytes)),
        Value::List(items) => {
            if let Some(bytes) = encode_f64_tensor_blob(&items) {
                (
                    value_blob_marker(field.to_string(), VALUE_BLOB_F64_TENSOR),
                    Some(bytes),
                )
            } else if let Some(bytes) = encode_f64_list_blob(&items) {
                (
                    value_blob_marker(field.to_string(), VALUE_BLOB_F64_LIST),
                    Some(bytes),
                )
            } else {
                (Value::List(items), None)
            }
        }
        other => (other, None),
    }
}

fn encode_f64_list_blob(items: &[Value]) -> Option<Vec<u8>> {
    if items.len() < MIN_NUMERIC_BLOB_VALUES {
        return None;
    }
    let mut out = Vec::with_capacity(items.len() * std::mem::size_of::<f64>());
    for item in items {
        out.extend_from_slice(&value_as_finite_f64(item)?.to_le_bytes());
    }
    Some(out)
}

fn encode_f64_tensor_blob(items: &[Value]) -> Option<Vec<u8>> {
    let rows = items.len();
    let Value::List(first) = items.first()? else {
        return None;
    };
    let cols = first.len();
    if rows == 0 || cols == 0 || rows.saturating_mul(cols) < MIN_NUMERIC_BLOB_VALUES {
        return None;
    }
    let rows_u32 = u32::try_from(rows).ok()?;
    let cols_u32 = u32::try_from(cols).ok()?;
    let mut out = Vec::with_capacity(8 + rows * cols * std::mem::size_of::<f64>());
    out.extend_from_slice(&rows_u32.to_le_bytes());
    out.extend_from_slice(&cols_u32.to_le_bytes());
    for row in items {
        let Value::List(values) = row else {
            return None;
        };
        if values.len() != cols {
            return None;
        }
        for value in values {
            out.extend_from_slice(&value_as_finite_f64(value)?.to_le_bytes());
        }
    }
    Some(out)
}

fn value_as_finite_f64(value: &Value) -> Option<f64> {
    let value = match value {
        Value::Int(value) => *value as f64,
        Value::Float(value) => *value,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

fn decode_json_field_value(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: DocId,
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
        _ => serde_json::from_str::<Value>(json_text)?,
    };
    if let Some(marker) = blob_marker_info(&value) {
        if let Some(decoded) = load_marked_document_blob(conn, table, doc_id, &marker)? {
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
    let marker_fields: Vec<(String, BlobMarker)> = document
        .iter()
        .filter_map(|(field, value)| blob_marker_info(value).map(|marker| (field.clone(), marker)))
        .collect();
    for (field, marker) in marker_fields {
        if let Some(value) = load_marked_document_blob(conn, table, doc_id, &marker)? {
            document.insert(field, value);
        }
    }
    Ok(())
}

fn load_marked_document_blob(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: DocId,
    marker: &BlobMarker,
) -> SQLiteResult<Option<Value>> {
    let field = match marker {
        BlobMarker::Bytes(field) | BlobMarker::F64List(field) | BlobMarker::F64Tensor(field) => {
            field.as_str()
        }
    };
    let Some(bytes) = load_document_blob(conn, table, doc_id, field)? else {
        return Ok(None);
    };
    let value = match marker {
        BlobMarker::Bytes(_) => Value::Bytes(bytes),
        BlobMarker::F64List(_) => decode_f64_list_blob(&bytes).unwrap_or(Value::Null),
        BlobMarker::F64Tensor(_) => decode_f64_tensor_blob(&bytes).unwrap_or(Value::Null),
    };
    Ok(Some(value))
}

fn decode_f64_list_blob(bytes: &[u8]) -> Option<Value> {
    if bytes.len() % std::mem::size_of::<f64>() != 0 {
        return None;
    }
    let values = bytes
        .chunks_exact(std::mem::size_of::<f64>())
        .map(|chunk| {
            let mut raw = [0_u8; std::mem::size_of::<f64>()];
            raw.copy_from_slice(chunk);
            Value::Float(f64::from_le_bytes(raw))
        })
        .collect();
    Some(Value::List(values))
}

fn decode_f64_tensor_blob(bytes: &[u8]) -> Option<Value> {
    if bytes.len() < 8 {
        return None;
    }
    let mut rows_raw = [0_u8; 4];
    rows_raw.copy_from_slice(&bytes[0..4]);
    let rows = u32::from_le_bytes(rows_raw) as usize;
    let mut cols_raw = [0_u8; 4];
    cols_raw.copy_from_slice(&bytes[4..8]);
    let cols = u32::from_le_bytes(cols_raw) as usize;
    let payload = &bytes[8..];
    if rows == 0 || cols == 0 || payload.len() != rows * cols * std::mem::size_of::<f64>() {
        return None;
    }
    let mut out = Vec::with_capacity(rows);
    for row in payload.chunks_exact(cols * std::mem::size_of::<f64>()) {
        let values = row
            .chunks_exact(std::mem::size_of::<f64>())
            .map(|chunk| {
                let mut raw = [0_u8; std::mem::size_of::<f64>()];
                raw.copy_from_slice(chunk);
                Value::Float(f64::from_le_bytes(raw))
            })
            .collect();
        out.push(Value::List(values));
    }
    Some(Value::List(out))
}

fn load_document_blob(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: DocId,
    field: &str,
) -> SQLiteResult<Option<Vec<u8>>> {
    conn.query_row(
        &format!(
            "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
             WHERE table_name = ?1 AND doc_id = ?2 AND field_name = ?3"
        ),
        params![table, doc_id as i64, field],
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
    conn.execute(
        &format!(
            "DELETE FROM {DOCUMENT_BLOBS_TABLE}
             WHERE table_name = ?1 AND doc_id = ?2 AND field_name = ?3"
        ),
        params![table, doc_id as i64, field],
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
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO {DOCUMENT_BLOBS_TABLE}
             (table_name, doc_id, field_name, bytes)
             VALUES (?1, ?2, ?3, ?4)"
        ),
        params![table, doc_id as i64, field, bytes],
    )?;
    Ok(())
}

impl DocumentStore for SQLiteDocumentStore {
    fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
        self.put_inner(doc_id, &document)?;
        Ok(())
    }

    fn get(&self, doc_id: DocId) -> Option<Document> {
        self.get_inner(doc_id).ok().flatten()
    }

    fn contains_doc_id(&self, doc_id: DocId) -> bool {
        self.conn
            .with(|c| {
                let found: Option<i64> = c
                    .prepare_cached(
                        "SELECT 1 FROM _documents
                         WHERE table_name = ?1 AND doc_id = ?2
                         LIMIT 1",
                    )?
                    .query_row(params![self.table, doc_id as i64], |r| r.get(0))
                    .optional()?;
                Ok(found.is_some())
            })
            .unwrap_or(false)
    }

    fn get_field(&self, doc_id: DocId, field: &str) -> Option<uqa_core::Value> {
        self.get_field_inner(doc_id, field).ok().flatten()
    }

    fn find_doc_id_by_field(&self, field: &str, value: &Value) -> Option<DocId> {
        self.find_doc_id_by_field_inner(field, value).ok().flatten()
    }

    fn get_fields_bulk(&self, doc_ids: &[DocId], field: &str) -> BTreeMap<DocId, Value> {
        let mut out: BTreeMap<DocId, Value> = doc_ids
            .iter()
            .copied()
            .map(|doc_id| (doc_id, Value::Null))
            .collect();
        if doc_ids.is_empty() {
            return out;
        }

        let path = sqlite_json_path(field);
        // Selective requests probe by id; wide requests (half the
        // table or more) sequential-scan once instead of issuing many
        // B-tree probes.
        if doc_ids.len() <= DOC_ID_IN_CHUNK || doc_ids.len() * 2 < self.len() {
            let leading = [
                rusqlite::types::Value::Text(self.table.clone()),
                rusqlite::types::Value::Text(path),
            ];
            let sql = format!(
                "SELECT doc_id, json_type(body, ?2), json_quote(json_extract(body, ?2))
                 FROM _documents
                 WHERE table_name = ?1 AND doc_id IN ({})",
                doc_id_in_placeholders(3, DOC_ID_IN_CHUNK)
            );
            let _ = self.conn.with(|c| {
                for chunk in doc_ids.chunks(DOC_ID_IN_CHUNK) {
                    let mut stmt = c.prepare_cached(&sql)?;
                    let bind = chunk_bind_values(&leading, chunk);
                    let mut rows = stmt.query(rusqlite::params_from_iter(bind))?;
                    while let Some(row) = rows.next()? {
                        let doc_id = row.get::<_, i64>(0)? as DocId;
                        let json_type = row.get::<_, Option<String>>(1)?;
                        let json_text = row.get::<_, String>(2)?;
                        if let Some(value) =
                            decode_json_field_value(c, &self.table, doc_id, json_type, &json_text)?
                        {
                            out.insert(doc_id, value);
                        }
                    }
                }
                Ok(())
            });
            return out;
        }

        let requested: BTreeSet<DocId> = doc_ids.iter().copied().collect();
        let _ = self.conn.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT doc_id, json_type(body, ?2), json_quote(json_extract(body, ?2))
                 FROM _documents
                 WHERE table_name = ?1
                 ORDER BY doc_id",
            )?;
            let mut rows = stmt.query(params![self.table, path])?;
            while let Some(row) = rows.next()? {
                let doc_id = row.get::<_, i64>(0)? as DocId;
                if !requested.contains(&doc_id) {
                    continue;
                }
                let json_type = row.get::<_, Option<String>>(1)?;
                let json_text = row.get::<_, String>(2)?;
                if let Some(value) =
                    decode_json_field_value(c, &self.table, doc_id, json_type, &json_text)?
                {
                    out.insert(doc_id, value);
                }
            }
            Ok(())
        });
        out
    }

    fn get_fields_multi(&self, doc_ids: &[DocId], fields: &[&str]) -> BTreeMap<DocId, Vec<Value>> {
        use std::fmt::Write as _;

        let mut out: BTreeMap<DocId, Vec<Value>> = BTreeMap::new();
        if doc_ids.is_empty() || fields.is_empty() {
            return out;
        }

        // One `json_type`/`json_extract` pair per field; the SQL text
        // depends only on the field count, so cached statements are
        // reused across queries touching different columns.
        let mut select_exprs = String::new();
        for i in 0..fields.len() {
            // Field paths start at ?2 (selective) / ?2..N+1 (scan).
            let idx = i + 2;
            let _ = write!(
                select_exprs,
                ", json_type(body, ?{idx}), json_quote(json_extract(body, ?{idx}))"
            );
        }
        let decode_row = |c: &rusqlite::Connection,
                          row: &rusqlite::Row<'_>|
         -> SQLiteResult<(DocId, Vec<Value>)> {
            let doc_id = row.get::<_, i64>(0)? as DocId;
            let mut values = Vec::with_capacity(fields.len());
            for i in 0..fields.len() {
                let json_type = row.get::<_, Option<String>>(1 + i * 2)?;
                let json_text = row.get::<_, String>(2 + i * 2)?;
                values.push(
                    decode_json_field_value(c, &self.table, doc_id, json_type, &json_text)?
                        .unwrap_or(Value::Null),
                );
            }
            Ok((doc_id, values))
        };

        if doc_ids.len() <= DOC_ID_IN_CHUNK || doc_ids.len() * 2 < self.len() {
            let mut leading = Vec::with_capacity(1 + fields.len());
            leading.push(rusqlite::types::Value::Text(self.table.clone()));
            for field in fields {
                leading.push(rusqlite::types::Value::Text(sqlite_json_path(field)));
            }
            let sql = format!(
                "SELECT doc_id{select_exprs} FROM _documents
                 WHERE table_name = ?1 AND doc_id IN ({})",
                doc_id_in_placeholders(2 + fields.len(), DOC_ID_IN_CHUNK)
            );
            let _ = self.conn.with(|c| {
                for chunk in doc_ids.chunks(DOC_ID_IN_CHUNK) {
                    let mut stmt = c.prepare_cached(&sql)?;
                    let bind = chunk_bind_values(&leading, chunk);
                    let mut rows = stmt.query(rusqlite::params_from_iter(bind))?;
                    while let Some(row) = rows.next()? {
                        let (doc_id, values) = decode_row(c, row)?;
                        out.insert(doc_id, values);
                    }
                }
                Ok(())
            });
            return out;
        }

        let requested: BTreeSet<DocId> = doc_ids.iter().copied().collect();
        let sql = format!(
            "SELECT doc_id{select_exprs} FROM _documents
             WHERE table_name = ?1
             ORDER BY doc_id"
        );
        let _ = self.conn.with(|c| {
            let mut stmt = c.prepare_cached(&sql)?;
            let mut bind: Vec<rusqlite::types::Value> = Vec::with_capacity(1 + fields.len());
            bind.push(rusqlite::types::Value::Text(self.table.clone()));
            for field in fields {
                bind.push(rusqlite::types::Value::Text(sqlite_json_path(field)));
            }
            let mut rows = stmt.query(rusqlite::params_from_iter(bind))?;
            while let Some(row) = rows.next()? {
                let doc_id = row.get::<_, i64>(0)? as DocId;
                if !requested.contains(&doc_id) {
                    continue;
                }
                let (doc_id, values) = decode_row(c, row)?;
                out.insert(doc_id, values);
            }
            Ok(())
        });
        out
    }

    fn get_many(&self, doc_ids: &[DocId]) -> BTreeMap<DocId, Document> {
        let mut out: BTreeMap<DocId, Document> = BTreeMap::new();
        if doc_ids.is_empty() {
            return out;
        }

        // Same probe-vs-scan split as `get_fields_bulk`.
        if doc_ids.len() <= DOC_ID_IN_CHUNK || doc_ids.len() * 2 < self.len() {
            let leading = [rusqlite::types::Value::Text(self.table.clone())];
            let sql = format!(
                "SELECT doc_id, body FROM _documents
                 WHERE table_name = ?1 AND doc_id IN ({})",
                doc_id_in_placeholders(2, DOC_ID_IN_CHUNK)
            );
            let _ = self.conn.with(|c| {
                for chunk in doc_ids.chunks(DOC_ID_IN_CHUNK) {
                    let mut stmt = c.prepare_cached(&sql)?;
                    let bind = chunk_bind_values(&leading, chunk);
                    let mut rows = stmt.query(rusqlite::params_from_iter(bind))?;
                    while let Some(row) = rows.next()? {
                        let doc_id = row.get::<_, i64>(0)? as DocId;
                        let body = row.get::<_, String>(1)?;
                        let mut document: Document = serde_json::from_str(&body)?;
                        hydrate_document_blobs(c, &self.table, doc_id, &mut document)?;
                        out.insert(doc_id, document);
                    }
                }
                Ok(())
            });
            return out;
        }

        let requested: BTreeSet<DocId> = doc_ids.iter().copied().collect();
        let _ = self.conn.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT doc_id, body FROM _documents
                 WHERE table_name = ?1
                 ORDER BY doc_id",
            )?;
            let mut rows = stmt.query(params![self.table])?;
            while let Some(row) = rows.next()? {
                let doc_id = row.get::<_, i64>(0)? as DocId;
                if !requested.contains(&doc_id) {
                    continue;
                }
                let body = row.get::<_, String>(1)?;
                let mut document: Document = serde_json::from_str(&body)?;
                hydrate_document_blobs(c, &self.table, doc_id, &mut document)?;
                out.insert(doc_id, document);
            }
            Ok(())
        });
        out
    }

    fn patch_fields(
        &mut self,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
    ) -> StorageBackendResult<bool> {
        Ok(self.patch_fields_inner(doc_id, updates)?)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.ensure_blob_table()?;
        self.conn.with(|c| {
            c.prepare_cached(&format!(
                "DELETE FROM {DOCUMENT_BLOBS_TABLE}
                 WHERE table_name = ?1 AND doc_id = ?2"
            ))?
            .execute(params![self.table, doc_id as i64])?;
            c.prepare_cached("DELETE FROM _documents WHERE table_name = ?1 AND doc_id = ?2")?
                .execute(params![self.table, doc_id as i64])?;
            Ok(())
        })?;
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.ensure_blob_table()?;
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

    fn doc_ids(&self) -> Vec<DocId> {
        self.conn
            .with(|c| {
                let mut stmt = c.prepare_cached(
                    "SELECT doc_id FROM _documents WHERE table_name = ?1 ORDER BY doc_id",
                )?;
                let rows = stmt.query_map(params![self.table], |r| r.get::<_, i64>(0))?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row? as DocId);
                }
                Ok(out)
            })
            .unwrap_or_default()
    }

    fn max_doc_id(&self) -> DocId {
        SQLiteDocumentStore::max_doc_id(self)
    }

    fn len(&self) -> usize {
        self.conn
            .with(|c| {
                let n: i64 = c
                    .prepare_cached("SELECT COUNT(*) FROM _documents WHERE table_name = ?1")?
                    .query_row(params![self.table], |r| r.get(0))?;
                Ok(n as usize)
            })
            .unwrap_or(0)
    }

    fn snapshot(&self) -> Arc<dyn DocumentStore> {
        Arc::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::catalog::Catalog;
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
        let got = s.get(1).unwrap();
        assert_eq!(got.get("title"), Some(&Value::Str("rust".into())));
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
        let got = s.get(1).unwrap();
        assert_eq!(got.get("title"), Some(&Value::Str("small".into())));
    }

    #[test]
    fn delete_removes_row() {
        let mut s = store();
        s.put(1, doc([("a", Value::Int(1))])).unwrap();
        s.delete(1).unwrap();
        assert!(s.get(1).is_none());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn doc_ids_sorted_ascending() {
        let mut s = store();
        s.put(3, Document::new()).unwrap();
        s.put(1, Document::new()).unwrap();
        s.put(2, Document::new()).unwrap();
        assert_eq!(s.doc_ids(), vec![1, 2, 3]);
    }

    #[test]
    fn get_field_reads_individual_field() {
        let mut s = store();
        s.put(
            7,
            doc([("year", Value::Int(2026)), ("flag", Value::Bool(true))]),
        )
        .unwrap();
        assert_eq!(s.get_field(7, "year"), Some(Value::Int(2026)));
        assert_eq!(s.get_field(7, "flag"), Some(Value::Bool(true)));
        assert_eq!(s.get_field(7, "missing"), None);
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

        let titles = s.get_fields_bulk(&[1, 2, 99], "title");
        assert_eq!(titles.get(&1), Some(&Value::Str("rust".into())));
        assert_eq!(titles.get(&2), Some(&Value::Str("sqlite".into())));
        assert_eq!(titles.get(&99), Some(&Value::Null));

        let payloads = s.get_fields_bulk(&[1, 2], "payload");
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
            s.find_doc_id_by_field("public_id", &Value::Str("m-9".into())),
            Some(9)
        );
        assert_eq!(
            s.find_doc_id_by_field("public_id", &Value::Str("missing".into())),
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

        let got = s.get(31).unwrap();
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
        assert_eq!(s.get_field(41, "bytes"), Some(Value::Bytes(vec![4, 5])));

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
            s.get_field(11, "bytes"),
            Some(Value::Bytes(vec![1, 2, 3, 4]))
        );
        let got = s.get(11).unwrap();
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

        assert_eq!(s.get_field(12, "embedding"), Some(embedding.clone()));
        assert_eq!(s.get_field(12, "tensor"), Some(tensor.clone()));

        let fields = s.get_fields_bulk(&[12, 99], "embedding");
        assert_eq!(fields.get(&12), Some(&embedding));
        assert_eq!(fields.get(&99), Some(&Value::Null));

        let got = s.get(12).unwrap();
        assert_eq!(got.get("embedding"), Some(&embedding));
        assert_eq!(got.get("tensor"), Some(&tensor));
        assert_eq!(got.get("title"), Some(&Value::Str("vector".into())));
    }

    #[test]
    fn legacy_inline_byte_arrays_are_rewritten_to_blob_storage_on_read() {
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

        let got = s.get(21).unwrap();
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
                assert!(body.contains(BLOB_MARKER_VALUE), "{body}");
                assert!(!body.contains("\"bytes\":[9,8,7]"), "{body}");

                let bytes: Vec<u8> = c.query_row(
                    &format!(
                        "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
                         WHERE table_name = 'articles'
                           AND doc_id = 21
                           AND field_name = 'bytes'"
                    ),
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(bytes, vec![9, 8, 7]);
                Ok(())
            })
            .unwrap();
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
