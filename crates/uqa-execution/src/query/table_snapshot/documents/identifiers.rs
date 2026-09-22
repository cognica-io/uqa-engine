//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-owned identity pages keep their merge buffers charged through internal consumers.

use super::{DocId, RetainedDocuments, StorageBackendError, StorageBackendResult};
use uqa_core::memory::{BudgetedVec, MemoryError};

impl RetainedDocuments {
    pub(in crate::query::table_snapshot) fn id_page(
        &self,
        after: Option<DocId>,
        limit: usize,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        let control = &self.0.control;
        control.check()?;
        let mut ids = BudgetedVec::new(control.memory());
        if limit == 0 {
            return Ok(ids);
        }
        let mut base = BudgetedVec::new(control.memory());
        let mut cursor = after;
        while base.len() < limit {
            control.check()?;
            let requested = (limit - base.len()).min(crate::DEFAULT_BATCH_SIZE);
            let page = self.0.source.next_doc_ids(cursor, requested)?;
            let Some(last) = page.last().copied() else {
                break;
            };
            if page.len() > requested
                || cursor.is_some_and(|cursor| page[0] <= cursor)
                || page.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(StorageBackendError::Other(
                    "query document page exceeds its request or does not advance in id order"
                        .into(),
                ));
            }
            // This adopts the provider's owned output boundary; allocation before that return remains the provider's responsibility.
            let bytes = page
                .capacity()
                .checked_mul(size_of::<DocId>())
                .ok_or(MemoryError::SizeOverflow)?;
            let _page_memory = control.memory().reserve(bytes)?;
            cursor = Some(last);
            for id in page {
                control.check()?;
                if !self.0.changes.contains_change(id) {
                    base.push(id)?;
                }
            }
        }
        let private = self
            .0
            .changes
            .changes_after(after)
            .take_while(|_| !control.cancellation().is_cancelled())
            .filter_map(|(id, present)| present.then_some(id))
            .take(limit);
        let (base, _base_memory) = base.into_parts();
        let mut base = base.into_iter().peekable();
        let mut private = private.peekable();
        while ids.len() < limit {
            control.check()?;
            let id = match (base.peek(), private.peek()) {
                (Some(left), Some(right)) if left < right => base.next(),
                (_, Some(_)) => private.next(),
                (Some(_), None) => base.next(),
                (None, None) => break,
            };
            if let Some(id) = id {
                ids.push(id)?;
            }
        }
        control.check()?;
        Ok(ids)
    }

    pub(super) fn selected_ids(
        &self,
        ids: &[DocId],
        private: bool,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        self.0.control.check()?;
        let mut selected = BudgetedVec::new(self.0.control.memory());
        for id in ids {
            self.0.control.check()?;
            if self.0.changes.contains_change(*id) == private {
                selected.push(*id)?;
            }
        }
        Ok(selected)
    }
}
