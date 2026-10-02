//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The cached count of a table's stored documents and the writes that keep it.
//!
//! A clean count equals the length of the table's document store in this session's view. Each row write knows whether it added or removed a document, so it keeps a clean count exact instead of discarding it; counting a persistent store reads every record of the table. Every other change to the store, and every rollback, discards the count, and the next reader counts the store again.

use std::sync::atomic::Ordering;

use uqa_sql::SQLError;

use crate::{Engine, TableState};

/// How one row write changed the documents its table stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DocumentCountChange {
    /// A document was stored where none was.
    Added,
    /// A stored document was removed.
    Removed,
    /// A stored document was replaced, or there was none to remove.
    Unchanged,
}

impl TableState {
    /// Keep a clean cached count equal to the store after one row write. A discarded count stays discarded.
    pub(crate) fn apply_document_count_change(&self, change: DocumentCountChange) {
        if self.doc_count_dirty.load(Ordering::Acquire) {
            return;
        }
        let update = |count: u64| match change {
            DocumentCountChange::Added => count.checked_add(1),
            DocumentCountChange::Removed => count.checked_sub(1),
            DocumentCountChange::Unchanged => Some(count),
        };
        if self
            .doc_count_cache
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, update)
            .is_err()
        {
            self.discard_document_count();
        }
    }

    /// Have the next reader count the store.
    pub(crate) fn discard_document_count(&self) {
        self.doc_count_dirty.store(true, Ordering::Release);
    }

    /// The length of the document store, counted only when no clean count is cached.
    pub(crate) fn stored_document_count(&self) -> Result<u64, SQLError> {
        if !self.doc_count_dirty.load(Ordering::Acquire) {
            return Ok(self.doc_count_cache.load(Ordering::Acquire));
        }
        let count = self
            .document_store
            .read()
            .len()
            .map_err(|error| SQLError::Internal(format!("read document count: {error}")))?;
        let count = u64::try_from(count)
            .map_err(|_| SQLError::Internal("document count exceeds u64".into()))?;
        self.doc_count_cache.store(count, Ordering::Release);
        self.doc_count_dirty.store(false, Ordering::Release);
        Ok(count)
    }
}

impl Engine {
    /// Discard the counts of persistent tables after a rollback: the stores lost the rolled-back row writes that the counts had followed. Memory and temporary tables restore their counts with their data.
    pub(crate) fn discard_persistent_document_counts(&self) {
        if self.storage.backend.is_none() {
            return;
        }
        for table in self.storage.tables.read().values() {
            if table.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                table.discard_document_count();
            }
        }
    }
}

#[cfg(test)]
mod tests;
