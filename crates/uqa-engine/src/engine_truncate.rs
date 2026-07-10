//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{DocId, Engine};

impl Engine {
    pub fn truncate_table(&self, name: &str) {
        let Some(t) = self.table(name) else {
            return;
        };
        // Snapshot the doc id set before grabbing any write locks so
        // we do not deadlock against the read guard inside the loop.
        let ids: Vec<DocId> = t.document_store.read().snapshot().doc_ids();
        for doc_id in ids {
            t.document_store.write().delete(doc_id);
            t.inverted_index.write().remove_document(doc_id);
            for idx in t.vector_indexes.write().values_mut() {
                idx.as_mut().delete(doc_id);
            }
        }
        *t.next_id.lock() = 1;
        Self::value_indexes_clear(&t);
        self.mark_column_stats_dirty(name, &t);
    }
}
