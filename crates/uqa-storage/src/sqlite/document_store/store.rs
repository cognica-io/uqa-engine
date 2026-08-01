//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Single-document reads, writes, patching, and scalar lookup.

use super::{
    blob_marker, decode_json_field_value, decode_legacy_document_body, delete_document_blob,
    document_id_from_sqlite, encode_document_blobs, encode_stored_value, hydrate_document_blobs,
    params, sqlite_doc_id, upsert_document_blob, BTreeMap, DocId, Document, ManagedConnection,
    OptionalExtension, SQLiteDocumentStore, SQLiteResult, StorageBackendResult, Value,
    DOCUMENT_BLOBS_TABLE,
};

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

    pub(super) fn put_inner(&self, doc_id: DocId, document: &Document) -> SQLiteResult<()> {
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

    pub(super) fn get_inner(&self, doc_id: DocId) -> SQLiteResult<Option<Document>> {
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

    pub(super) fn get_field_inner(
        &self,
        doc_id: DocId,
        field: &str,
    ) -> SQLiteResult<Option<Value>> {
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

    pub(super) fn find_doc_id_by_field_inner(
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

    pub(super) fn patch_fields_inner(
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
