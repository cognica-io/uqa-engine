//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-owned identity pages keep their merge buffers charged through internal consumers.

use super::{DocId, RetainedDocuments, StorageBackendError, StorageBackendResult};
use uqa_core::memory::BudgetedVec;
use uqa_storage::{document_store::read_document_ids, read_control::StorageReadControl};

enum BorrowedPage {
    Unsupported,
    Empty,
    Last(DocId),
}

impl RetainedDocuments {
    pub(in crate::query::table_snapshot) fn id_page(
        &self,
        after: Option<DocId>,
        limit: usize,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        self.merged_id_page(after, limit, &self.0.control, true)
    }

    pub(super) fn id_page_controlled(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        self.merged_id_page(after, limit, control, false)
    }

    fn merged_id_page(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
        borrowed: bool,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        self.0.control.check()?;
        control.check()?;
        let mut ids = BudgetedVec::new(control.memory());
        if limit == 0 {
            return Ok(ids);
        }
        let mut base = BudgetedVec::new(control.memory());
        let mut cursor = after;
        while base.len() < limit {
            control.check()?;
            self.0.control.check()?;
            let requested = (limit - base.len()).min(crate::DEFAULT_BATCH_SIZE);
            if borrowed {
                match self.append_borrowed_ids(cursor, requested, &mut base, control)? {
                    BorrowedPage::Empty => break,
                    BorrowedPage::Last(last) => {
                        cursor = Some(last);
                        continue;
                    }
                    BorrowedPage::Unsupported => {}
                }
            }
            let page = read_document_ids(self.0.source.as_ref(), cursor, requested, control)?;
            let Some(last) = page.last().copied() else {
                break;
            };
            cursor = Some(last);
            for id in page.iter().copied() {
                control.check()?;
                self.0.control.check()?;
                if !self.0.changes.contains_change(id) {
                    base.push(id)?;
                }
            }
        }
        let private = self
            .0
            .changes
            .changes_after(after)
            .take_while(|_| {
                !control.cancellation().is_cancelled()
                    && !self.0.control.cancellation().is_cancelled()
            })
            .filter_map(|(id, present)| present.then_some(id))
            .take(limit);
        let (base, _base_memory) = base.into_parts();
        let mut base = base.into_iter().peekable();
        let mut private = private.peekable();
        while ids.len() < limit {
            control.check()?;
            self.0.control.check()?;
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
        self.0.control.check()?;
        Ok(ids)
    }

    fn append_borrowed_ids(
        &self,
        after: Option<DocId>,
        limit: usize,
        ids: &mut BudgetedVec<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BorrowedPage> {
        let mut count = 0;
        let mut last = after;
        let mut failure = None;
        let result = self.0.source.for_each_next_fields(after, limit, &[], &mut |id, _| {
            if failure.is_some() {
                return false;
            }
            let result = (|| {
                control.check()?;
                self.0.control.check()?;
                if count == limit || last.is_some_and(|last| id <= last) {
                    return Err(StorageBackendError::Other(
                        "query document cursor exceeds its request or does not advance in id order".into(),
                    ));
                }
                count += 1;
                last = Some(id);
                if !self.0.changes.contains_change(id) {
                    ids.push(id)?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                failure = Some(error);
                return false;
            }
            true
        });
        if let Some(error) = failure {
            return Err(error);
        }
        let result = result?;
        control.check()?;
        self.0.control.check()?;
        match result {
            Some(reported) if reported == count => Ok(if count == 0 {
                BorrowedPage::Empty
            } else {
                BorrowedPage::Last(last.expect("visited identity"))
            }),
            None if count == 0 => Ok(BorrowedPage::Unsupported),
            _ => Err(StorageBackendError::Other(
                "query document cursor reports a count inconsistent with its callbacks".into(),
            )),
        }
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
