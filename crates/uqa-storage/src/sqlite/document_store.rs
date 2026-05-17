//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed [`DocumentStore`].

use std::collections::BTreeMap;
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use uqa_core::{DocId, Value};

use crate::document_store::{Document, DocumentStore};
use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult};

const DOCUMENT_BLOBS_TABLE: &str = "_document_blobs";
const BLOB_MARKER_TYPE: &str = "$uqa_type";
const BLOB_MARKER_VALUE: &str = "document_blob";
const BLOB_MARKER_FIELD: &str = "field";

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
                let id: Option<i64> = c.query_row(
                    "SELECT MAX(doc_id) FROM _documents WHERE table_name = ?1",
                    params![self.table],
                    |r| r.get(0),
                )?;
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
        self.ensure_blob_table()?;
        self.conn.with(|c| {
            c.execute(
                &format!(
                    "DELETE FROM {DOCUMENT_BLOBS_TABLE}
                     WHERE table_name = ?1 AND doc_id = ?2"
                ),
                params![self.table, doc_id as i64],
            )?;
            c.execute(
                "INSERT OR REPLACE INTO _documents (table_name, doc_id, body)
                 VALUES (?1, ?2, ?3)",
                params![self.table, doc_id as i64, body],
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
            Ok(())
        })
    }

    fn get_inner(&self, doc_id: DocId) -> SQLiteResult<Option<Document>> {
        self.ensure_blob_table()?;
        self.conn.with(|c| {
            let body: Option<String> = c
                .query_row(
                    "SELECT body FROM _documents
                     WHERE table_name = ?1 AND doc_id = ?2",
                    params![self.table, doc_id as i64],
                    |r| r.get(0),
                )
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
        self.ensure_blob_table()?;
        let path = sqlite_json_path(field);
        self.conn.with(|c| {
            let row: Option<(Option<String>, String)> = c
                .query_row(
                    "SELECT json_type(body, ?3), json_quote(json_extract(body, ?3))
                     FROM _documents
                     WHERE table_name = ?1 AND doc_id = ?2",
                    params![self.table, doc_id as i64, path],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((json_type, json_text)) = row else {
                return Ok(None);
            };
            let Some(json_type) = json_type else {
                return Ok(None);
            };
            let mut value = match json_type.as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                "null" => Value::Null,
                _ => serde_json::from_str::<Value>(&json_text)?,
            };
            if let Some(blob_field) = marker_field(&value) {
                if let Some(bytes) = load_document_blob(c, &self.table, doc_id, blob_field)? {
                    value = Value::Bytes(bytes);
                }
            }
            Ok(Some(value))
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
        self.ensure_blob_table()?;
        self.conn.with(|c| {
            let exists: Option<i64> = c
                .query_row(
                    "SELECT 1 FROM _documents
                     WHERE table_name = ?1 AND doc_id = ?2
                     LIMIT 1",
                    params![self.table, doc_id as i64],
                    |r| r.get(0),
                )
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
                        delete_document_blob(c, &self.table, doc_id, field)?;
                        let json = serde_json::to_string(other)?;
                        c.execute(
                            "UPDATE _documents SET body = json_set(body, ?3, json(?4))
                             WHERE table_name = ?1 AND doc_id = ?2",
                            params![self.table, doc_id as i64, path, json],
                        )?;
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
        match value {
            Value::Bytes(bytes) => {
                blobs.push((field.clone(), bytes));
                stored.insert(field.clone(), blob_marker(field));
            }
            other => {
                stored.insert(field, other);
            }
        }
    }
    (stored, blobs)
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

fn marker_field(value: &Value) -> Option<&str> {
    let Value::Map(map) = value else {
        return None;
    };
    match (map.get(BLOB_MARKER_TYPE), map.get(BLOB_MARKER_FIELD)) {
        (Some(Value::Str(kind)), Some(Value::Str(field))) if kind == BLOB_MARKER_VALUE => {
            Some(field)
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
    let marker_fields: Vec<(String, String)> = document
        .iter()
        .filter_map(|(field, value)| {
            marker_field(value).map(|blob_field| (field.clone(), blob_field.to_string()))
        })
        .collect();
    for (field, blob_field) in marker_fields {
        let bytes: Option<Vec<u8>> = conn
            .query_row(
                &format!(
                    "SELECT bytes FROM {DOCUMENT_BLOBS_TABLE}
                     WHERE table_name = ?1 AND doc_id = ?2 AND field_name = ?3"
                ),
                params![table, doc_id as i64, blob_field],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(bytes) = bytes {
            document.insert(field, Value::Bytes(bytes));
        }
    }
    Ok(())
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
    fn put(&mut self, doc_id: DocId, document: Document) {
        let _ = self.put_inner(doc_id, &document);
    }

    fn get(&self, doc_id: DocId) -> Option<Document> {
        self.get_inner(doc_id).ok().flatten()
    }

    fn get_field(&self, doc_id: DocId, field: &str) -> Option<uqa_core::Value> {
        self.get_field_inner(doc_id, field).ok().flatten()
    }

    fn find_doc_id_by_field(&self, field: &str, value: &Value) -> Option<DocId> {
        self.find_doc_id_by_field_inner(field, value).ok().flatten()
    }

    fn patch_fields(&mut self, doc_id: DocId, updates: &BTreeMap<String, Value>) -> bool {
        self.patch_fields_inner(doc_id, updates).unwrap_or(false)
    }

    fn delete(&mut self, doc_id: DocId) {
        let _ = self.ensure_blob_table();
        let _ = self.conn.with(|c| {
            c.execute(
                &format!(
                    "DELETE FROM {DOCUMENT_BLOBS_TABLE}
                     WHERE table_name = ?1 AND doc_id = ?2"
                ),
                params![self.table, doc_id as i64],
            )?;
            c.execute(
                "DELETE FROM _documents WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id as i64],
            )?;
            Ok(())
        });
    }

    fn clear(&mut self) {
        let _ = self.ensure_blob_table();
        let _ = self.conn.with(|c| {
            c.execute(
                &format!("DELETE FROM {DOCUMENT_BLOBS_TABLE} WHERE table_name = ?1"),
                params![self.table],
            )?;
            c.execute(
                "DELETE FROM _documents WHERE table_name = ?1",
                params![self.table],
            )?;
            Ok(())
        });
    }

    fn doc_ids(&self) -> Vec<DocId> {
        self.conn
            .with(|c| {
                let mut stmt = c.prepare(
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
                let n: i64 = c.query_row(
                    "SELECT COUNT(*) FROM _documents WHERE table_name = ?1",
                    params![self.table],
                    |r| r.get(0),
                )?;
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
        s.put(1, doc([("title", Value::Str("rust".into()))]));
        let got = s.get(1).unwrap();
        assert_eq!(got.get("title"), Some(&Value::Str("rust".into())));
    }

    #[test]
    fn delete_removes_row() {
        let mut s = store();
        s.put(1, doc([("a", Value::Int(1))]));
        s.delete(1);
        assert!(s.get(1).is_none());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn doc_ids_sorted_ascending() {
        let mut s = store();
        s.put(3, Document::new());
        s.put(1, Document::new());
        s.put(2, Document::new());
        assert_eq!(s.doc_ids(), vec![1, 2, 3]);
    }

    #[test]
    fn get_field_reads_individual_field() {
        let mut s = store();
        s.put(
            7,
            doc([("year", Value::Int(2026)), ("flag", Value::Bool(true))]),
        );
        assert_eq!(s.get_field(7, "year"), Some(Value::Int(2026)));
        assert_eq!(s.get_field(7, "flag"), Some(Value::Bool(true)));
        assert_eq!(s.get_field(7, "missing"), None);
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
        );
        s.put(
            9,
            doc([
                ("public_id", Value::Str("m-9".into())),
                ("content", Value::Str("target".into())),
            ]),
        );

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
        );

        let updates = BTreeMap::from([
            ("content".to_string(), Value::Str("new".into())),
            ("token_count".to_string(), Value::Null),
        ]);
        assert!(s.patch_fields(31, &updates));

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
        );

        let updates = BTreeMap::from([("bytes".to_string(), Value::Bytes(vec![4, 5]))]);
        assert!(s.patch_fields(41, &updates));
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
        );

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
        s.put(1, doc([("bytes", Value::Bytes(vec![1]))]));
        s.put(2, doc([("bytes", Value::Bytes(vec![2]))]));

        s.delete(1);
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

        s.clear();
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
