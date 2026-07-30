//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{DocId, Engine, SQLError};
use crate::engine_table_storage::document_store_write_error;

impl Engine {
    pub fn truncate_table(&self, name: &str) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| engine.truncate_table_inner(name))
    }

    fn truncate_table_inner(&self, name: &str) -> Result<(), SQLError> {
        let table_name = self
            .try_resolve_table_name(name)
            .map_err(|error| SQLError::Internal(format!("resolve table `{name}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(name.to_string()))?;
        let t = self.require_table(&table_name)?;
        // Snapshot the doc id set before grabbing any write locks so
        // we do not deadlock against the read guard inside the loop.
        let ids: Vec<DocId> = t
            .document_store
            .read()
            .doc_ids()
            .map_err(|error| SQLError::Internal(format!("read document ids: {error}")))?;
        for doc_id in ids {
            t.document_store
                .write()
                .delete(doc_id)
                .map_err(|err| document_store_write_error(&err))?;
            t.inverted_index
                .write()
                .remove_document(doc_id)
                .map_err(|error| SQLError::Internal(format!("remove indexed document: {error}")))?;
            for idx in t.vector_indexes.write().values_mut() {
                idx.as_mut().delete(doc_id).map_err(|error| {
                    SQLError::Internal(format!("delete indexed vector: {error}"))
                })?;
            }
        }
        *t.next_id.lock() = 1;
        self.value_indexes_truncate(&table_name, &t)?;
        self.mark_column_stats_dirty(&table_name, &t)
            .map_err(|err| SQLError::Internal(format!("invalidate column stats: {err}")))?;
        Ok(())
    }
}
