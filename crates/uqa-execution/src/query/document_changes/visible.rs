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
        let mut changes = self.changes.changes_after(after);
        let mut private = changes.next().transpose()?;
        let mut base = BudgetedVec::new(control.memory());
        let mut position = 0;
        let mut cursor = after;
        let mut exhausted = false;
        while ids.len() < limit {
            control.check()?;
            self.control.check()?;
            if position == base.len() && !exhausted {
                base = BudgetedVec::new(control.memory());
                position = 0;
                let requested = (limit - ids.len()).min(crate::DEFAULT_BATCH_SIZE);
                let page = if borrowed {
                    self.append_borrowed_ids(cursor, requested, &mut base, control)?
                } else {
                    BorrowedPage::Unsupported
                };
                match page {
                    BorrowedPage::Empty => exhausted = true,
                    BorrowedPage::Last(last) => cursor = Some(last),
                    BorrowedPage::Unsupported => {
                        base = read_document_ids(self.source, cursor, requested, control)?;
                        if let Some(last) = base.last().copied() {
                            cursor = Some(last);
                        } else {
                            exhausted = true;
                        }
                    }
                }
            }
            control.check()?;
            self.control.check()?;
            let stored = base.get(position).copied();
            match (stored, private) {
                (Some(id), Some((changed, _))) if id < changed => {
                    ids.push(id)?;
                    position += 1;
                }
                (_, Some((id, present))) => {
                    position += usize::from(stored == Some(id));
                    if present {
                        ids.push(id)?;
                        if ids.len() == limit {
                            break;
                        }
                    }
                    private = changes.next().transpose()?;
                }
                (Some(id), None) => {
                    ids.push(id)?;
                    position += 1;
                }
                (None, None) => break,
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
                ids.push(id)?;
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
