//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed [`DocumentStore`].

use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use uqa_core::DocId;

use crate::document_store::{Document, DocumentStore};
use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult};

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
        let body = serde_json::to_string(&document)?;
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _documents (table_name, doc_id, body)
                 VALUES (?1, ?2, ?3)",
                params![self.table, doc_id as i64, body],
            )?;
            Ok(())
        })
    }

    fn get_inner(&self, doc_id: DocId) -> SQLiteResult<Option<Document>> {
        self.conn.with(|c| {
            let body: Option<String> = c
                .query_row(
                    "SELECT body FROM _documents
                     WHERE table_name = ?1 AND doc_id = ?2",
                    params![self.table, doc_id as i64],
                    |r| r.get(0),
                )
                .optional()?;
            match body {
                Some(s) => Ok(Some(serde_json::from_str(&s)?)),
                None => Ok(None),
            }
        })
    }
}

impl DocumentStore for SQLiteDocumentStore {
    fn put(&mut self, doc_id: DocId, document: Document) {
        let _ = self.put_inner(doc_id, &document);
    }

    fn get(&self, doc_id: DocId) -> Option<Document> {
        self.get_inner(doc_id).ok().flatten()
    }

    fn get_field(&self, doc_id: DocId, field: &str) -> Option<uqa_core::Value> {
        let doc = self.get_inner(doc_id).ok().flatten()?;
        doc.get(field).cloned()
    }

    fn delete(&mut self, doc_id: DocId) {
        let _ = self.conn.with(|c| {
            c.execute(
                "DELETE FROM _documents WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id as i64],
            )?;
            Ok(())
        });
    }

    fn clear(&mut self) {
        let _ = self.conn.with(|c| {
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
}
