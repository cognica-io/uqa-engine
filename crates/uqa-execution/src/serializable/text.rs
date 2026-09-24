//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical text reads and evaluated writes retain their original transaction and field identities.

use std::{ops::Bound, sync::Arc};
use uqa_core::{memory::BudgetedVec, DocId};
use uqa_sql::ast::ColumnDef;
use uqa_storage::{
    mvcc::{SerializableKeySpace, SerializablePredicate},
    StorageBackendResult, TokenTermKey,
};

use super::SerializableRelationRead;

mod key;
mod read;
mod write;
pub use read::{observe_snapshot, ObservedTextIndex};
pub use write::{add_document, add_documents, remove_document};

const POSTING: u8 = 0;
const DOCUMENT: u8 = 1;
const STATISTICS: u8 = 2;

#[derive(Clone)]
struct TextObservation {
    read: SerializableRelationRead,
    columns: Arc<Vec<ColumnDef>>,
}

impl TextObservation {
    fn field_key(&self, kind: u8, field: &str) -> StorageBackendResult<BudgetedVec<u8>> {
        key::field(&self.columns, kind, field, &self.read.control)
    }

    fn term_key(&self, field: &str, term: &TokenTermKey) -> StorageBackendResult<BudgetedVec<u8>> {
        key::term(&self.columns, field, term, &self.read.control)
    }

    fn point(&self, key: &[u8]) -> StorageBackendResult<()> {
        self.observe(SerializablePredicate::point(
            self.read.object,
            SerializableKeySpace::Text,
            key,
        ))
    }

    fn prefix(&self, prefix: &[u8]) -> StorageBackendResult<()> {
        let mut upper = BudgetedVec::new(self.read.control.memory());
        upper.extend_from_slice(prefix)?;
        while upper.last() == Some(&u8::MAX) {
            upper.pop();
        }
        let end = if let Some(last) = upper.last_mut() {
            *last += 1;
            Bound::Excluded(&upper[..])
        } else {
            Bound::Unbounded
        };
        self.observe(SerializablePredicate::range(
            self.read.object,
            SerializableKeySpace::Text,
            Bound::Included(prefix),
            end,
        ))
    }

    fn term(
        &self,
        field: &str,
        term: &TokenTermKey,
        doc: Option<DocId>,
    ) -> StorageBackendResult<()> {
        let mut key = self.term_key(field, term)?;
        if let Some(doc) = doc {
            key.extend_from_slice(&doc.to_be_bytes())?;
            self.point(&key)
        } else {
            self.prefix(&key)
        }
    }

    fn scalar_term(&self, field: &str, term: &str, doc: Option<DocId>) -> StorageBackendResult<()> {
        let term = TokenTermKey::from_text_budgeted(term, self.read.control.memory(), || {
            self.read.control.check()
        })?;
        self.term(field, &term, doc)
    }

    fn document(&self, field: &str, doc: DocId) -> StorageBackendResult<()> {
        let mut key = self.field_key(DOCUMENT, field)?;
        key.extend_from_slice(&doc.to_be_bytes())?;
        self.point(&key)
    }

    fn statistics(&self, field: &str) -> StorageBackendResult<()> {
        self.point(&self.field_key(STATISTICS, field)?)
    }

    fn field_range(&self, kind: u8, field: Option<&str>) -> StorageBackendResult<()> {
        match field {
            Some(field) => self.prefix(&self.field_key(kind, field)?),
            None => self.prefix(&[kind]),
        }
    }

    fn all(&self) -> StorageBackendResult<()> {
        self.prefix(&[])
    }

    fn observe(&self, predicate: SerializablePredicate<'_>) -> StorageBackendResult<()> {
        self.read
            .context
            .observe_read(predicate, &self.read.control)
            .map_err(uqa_storage::mvcc::VersionError::into_storage_error)
    }
}
