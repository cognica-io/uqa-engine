//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Merge base and command-visible identity pages without materializing the complete relation.

use super::DocumentChanges;
use uqa_core::{memory::BudgetedVec, DocId};
use uqa_storage::{
    document_store::read_document_ids, read_control::StorageReadControl, DocumentStore,
    StorageBackendError, StorageBackendResult,
};

pub struct VisibleDocumentIds<'a> {
    pub source: &'a dyn DocumentStore,
    pub changes: &'a DocumentChanges,
    pub control: &'a StorageReadControl,
}

enum BorrowedPage {
    Unsupported,
    Empty,
    Last(DocId),
}

impl VisibleDocumentIds<'_> {
    pub fn page(
        &self,
        after: Option<DocId>,
        limit: usize,
        control: &StorageReadControl,
        borrowed: bool,
    ) -> StorageBackendResult<BudgetedVec<DocId>> {
        self.control.check()?;
        control.check()?;
        let mut ids = BudgetedVec::new(control.memory());
        if limit == 0 {
            return Ok(ids);
        }
        let mut base = BudgetedVec::new(control.memory());
        let mut cursor = after;
        while base.len() < limit {
            control.check()?;
            self.control.check()?;
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
            let page = read_document_ids(self.source, cursor, requested, control)?;
            let Some(last) = page.last().copied() else {
                break;
            };
            cursor = Some(last);
            for id in page.iter().copied() {
                control.check()?;
                self.control.check()?;
                if !self.changes.contains_change(id)? {
                    base.push(id)?;
                }
            }
        }
        let mut private = BudgetedVec::new(control.memory());
        for change in self.changes.changes_after(after) {
            control.check()?;
            self.control.check()?;
            if private.len() == limit {
                break;
            }
            let (id, present) = change?;
            if present {
                private.push(id)?;
            }
        }
        let (base, _base_memory) = base.into_parts();
        let mut base = base.into_iter().peekable();
        let (private, _private_memory) = private.into_parts();
        let mut private = private.into_iter().peekable();
        while ids.len() < limit {
            control.check()?;
            self.control.check()?;
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
        self.control.check()?;
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
        let result = self.source.for_each_next_fields(after, limit, &[], &mut |id, _| {
            if failure.is_some() {
                return false;
            }
            let result = (|| {
                control.check()?;
                self.control.check()?;
                if count == limit || last.is_some_and(|last| id <= last) {
                    return Err(StorageBackendError::Other(
                        "query document cursor exceeds its request or does not advance in id order".into(),
                    ));
                }
                count += 1;
                last = Some(id);
                if !self.changes.contains_change(id)? {
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
        self.control.check()?;
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
}
