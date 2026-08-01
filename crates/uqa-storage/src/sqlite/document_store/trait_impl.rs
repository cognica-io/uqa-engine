//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete `DocumentStore` query, mutation, iteration, and snapshot contract.

use super::{
    allocation_error, blob_marker_info, chunk_bind_values, decode_legacy_document_body,
    doc_id_in_placeholders, document_id_from_sqlite, hydrate_document_blobs,
    load_marked_document_blob, params, read_doc_id, should_probe_doc_ids, sorted_unique_doc_ids,
    sqlite_doc_id, take_requested_field, Arc, BTreeMap, DocId, Document, DocumentStore,
    OptionalExtension, SQLiteDocumentStore, SQLiteError, SQLiteResult, StorageBackendResult, Value,
    DOCUMENT_BLOBS_TABLE, DOC_ID_IN_CHUNK,
};

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
