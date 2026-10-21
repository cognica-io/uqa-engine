//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read previously evaluated keys on one provider boundary without collecting their population.

use super::{decode_doc_id, decode_value, native, SQLiteBTreeIndexStore, SQLiteValueIndexKey};
use rusqlite::{params, OptionalExtension};
use uqa_core::{DocId, Value};
use uqa_storage::{StorageBackendResult, ValueIndexKey};

impl SQLiteBTreeIndexStore {
    pub fn visit_entries(
        &self,
        table: &str,
        field: &ValueIndexKey,
        visit: &mut dyn FnMut(DocId, Value) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<bool> {
        // A consumer's error belongs to that consumer, not the SQLite diagnostic adapter.
        let mut failure = None;
        let mut capture = |id, value| match visit(id, value) {
            Ok(()) => Ok(()),
            Err(error) => {
                failure = Some(error);
                Err(super::SQLiteError::StorageBackend(
                    "index entry consumer stopped".into(),
                ))
            }
        };
        let result = if let Some(snapshot) = self.conn.native_snapshot()? {
            native::visit_entries(&snapshot, table, field, &mut capture)
        } else {
            self.conn.with(|conn| {
                let exists = conn
                    .prepare_cached(
                        "SELECT 1 FROM _btree_indexes WHERE table_name = ?1 AND field = ?2",
                    )?
                    .query_row(params![table, SQLiteValueIndexKey(field)], |_| Ok(()))
                    .optional()?
                    .is_some();
                if !exists {
                    return Ok(false);
                }
                let mut statement = conn.prepare_cached(
                    "SELECT doc_id, value_json FROM _btree_index_entries
                     WHERE table_name = ?1 AND field = ?2 ORDER BY doc_id",
                )?;
                let mut rows = statement.query(params![table, SQLiteValueIndexKey(field)])?;
                while let Some(row) = rows.next()? {
                    let id = decode_doc_id(row.get(0)?)?;
                    let encoded: String = row.get(1)?;
                    capture(id, decode_value(&encoded)?)?;
                }
                Ok(true)
            })
        };
        if let Some(error) = failure {
            return Err(error);
        }
        result.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests;
