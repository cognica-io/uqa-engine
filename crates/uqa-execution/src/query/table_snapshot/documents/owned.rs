//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owned projections hold admitted output through batch handoff or the consuming callback.

use super::{
    BTreeMap, BudgetedVec, DocId, MemoryReservation, RetainedDocuments, StorageBackendError,
    StorageBackendResult, StoredDocument, Value,
};

impl RetainedDocuments {
    pub(super) fn read_owned_row(
        &self,
        id: DocId,
        memory: &mut MemoryReservation,
    ) -> StorageBackendResult<Option<StoredDocument>> {
        self.0.control.check()?;
        let private = self.0.changes.contains_change(id);
        let source: &dyn uqa_storage::DocumentStore = if private {
            &self.0.changes
        } else {
            self.0.source.as_ref()
        };
        let mut page =
            uqa_storage::document_store::read_stored_documents(source, &[id], &self.0.control)?;
        let row = page.pop().expect("one validated whole-row slot");
        drop(page);
        let Some(row) = row else {
            return Ok(None);
        };
        let (row, allocation) = row.into_budgeted(&self.0.control)?.into_parts();
        memory.absorb(allocation);
        let row = if private {
            self.0.private_layout.complete_private(row, memory)
        } else {
            self.0.layout.adapt_base(row, memory)
        }
        .map_err(super::layout_error)?;
        self.0.control.check()?;
        Ok(Some(row))
    }

    fn copy_values(
        &self,
        values: &[&Value],
        memory: &mut MemoryReservation,
    ) -> StorageBackendResult<Vec<Value>> {
        self.0.control.check()?;
        let mut copied = BudgetedVec::new(self.0.control.memory());
        copied.reserve(values.len())?;
        for value in values {
            let (value, allocation) = value
                .clone_budgeted(self.0.control.memory(), self.0.control.cancellation())
                .map_err(|error| match error {
                    uqa_core::ValueRetentionError::Memory(error) => {
                        StorageBackendError::Memory(error)
                    }
                    uqa_core::ValueRetentionError::Cancelled(error) => {
                        StorageBackendError::Cancelled(error)
                    }
                })?
                .into_parts();
            memory.absorb(allocation);
            copied.push(value)?;
        }
        self.0.control.check()?;
        let (copied, allocation) = copied.into_parts();
        memory.absorb(allocation);
        Ok(copied)
    }

    pub(super) fn copy_projected_rows(
        &self,
        ids: &[DocId],
        fields: &[&str],
    ) -> StorageBackendResult<BTreeMap<DocId, Vec<Value>>> {
        let mut memory = self.0.control.memory().empty_reservation();
        let mut rows = BTreeMap::new();
        let mut failure = None;
        let read = self.visit_projection(ids, fields, &mut |id, present, values| {
            // A fixed view gives every duplicate identity the same values; the owned map needs only one copy.
            if !present || rows.contains_key(&id) {
                return true;
            }
            let copied = memory
                .grow(size_of::<(DocId, Vec<Value>)>())
                .map_err(StorageBackendError::from)
                .and_then(|()| self.copy_values(values, &mut memory));
            match copied {
                Ok(values) => {
                    rows.insert(id, values);
                    true
                }
                Err(error) => {
                    failure = Some(error);
                    false
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        read?;
        self.0.control.check()?;
        Ok(rows)
    }

    pub(super) fn visit_owned_projection(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, Vec<Value>) -> bool,
    ) -> StorageBackendResult<()> {
        let mut failure = None;
        let read = self.visit_projection(ids, fields, &mut |id, _, values| {
            let mut memory = self.0.control.memory().empty_reservation();
            match self.copy_values(values, &mut memory) {
                Ok(values) => visitor(id, values),
                Err(error) => {
                    failure = Some(error);
                    false
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        read
    }
}
