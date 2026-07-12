//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{DocId, Engine, SQLError};
use crate::engine_table_storage::document_store_write_error;

impl Engine {
    pub fn truncate_table(&self, name: &str) -> Result<(), SQLError> {
        let Some(t) = self.table(name) else {
            return Ok(());
        };
        // Snapshot the doc id set before grabbing any write locks so
        // we do not deadlock against the read guard inside the loop.
        let ids: Vec<DocId> = t.document_store.read().snapshot().doc_ids();
        for doc_id in ids {
            t.document_store
                .write()
                .delete(doc_id)
                .map_err(|err| document_store_write_error(&err))?;
            t.inverted_index.write().remove_document(doc_id);
            for idx in t.vector_indexes.write().values_mut() {
                idx.as_mut().delete(doc_id);
            }
        }
        *t.next_id.lock() = 1;
        self.value_indexes_truncate(name, &t)?;
        self.mark_column_stats_dirty(name, &t);
        Ok(())
    }
}
