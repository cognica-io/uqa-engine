//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate a provider's bounded identity output without detaching its memory reservation.

use super::{
    BudgetedVec, DocId, DocumentStore, StorageBackendError, StorageBackendResult,
    StorageReadControl,
};

#[cfg(test)]
mod tests;

/// Request one ordered identity page from the same selected document view. A provider cannot replace the caller's allowance, exceed the requested count or return a nonadvancing cursor. Errors and cancellation return no partial page.
pub fn read_document_ids(
    store: &dyn DocumentStore,
    after: Option<DocId>,
    limit: usize,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<DocId>> {
    control.check()?;
    if limit == 0 {
        return Ok(BudgetedVec::new(control.memory()));
    }
    let page = store.next_doc_ids_controlled(after, limit, control)?;
    control.check()?;
    if !page.budget().shares_allowance(control.memory()) {
        return Err(StorageBackendError::Other(
            "document identity page belongs to a different allowance".into(),
        ));
    }
    if page.len() > limit {
        return Err(StorageBackendError::Other(
            "document identity page exceeds its requested count".into(),
        ));
    }
    let mut previous = after;
    for &id in page.iter() {
        control.check()?;
        if previous.is_some_and(|previous| id <= previous) {
            return Err(StorageBackendError::Other(
                "document identity page does not advance in id order".into(),
            ));
        }
        previous = Some(id);
    }
    control.check()?;
    Ok(page)
}
