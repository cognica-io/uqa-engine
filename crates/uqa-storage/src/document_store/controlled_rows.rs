//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Whole-row reads retain provider buffers and decoded payloads through their consumers.

use uqa_core::{
    memory::{Budgeted, BudgetedString, BudgetedVec},
    Value,
};

use super::{Document, DocumentStore, RetainedDocumentFields, RetainedStoredDocument};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

/// One fixed-view result for each requested identity, including missing rows and duplicates. Page capacity and present payloads retain their own reservations.
pub type RetainedDocumentPage = BudgetedVec<Option<RetainedStoredDocument>>;

/// Validate one whole-row page without releasing its provider reservations or copying decoded payloads.
pub fn read_stored_documents(
    source: &dyn DocumentStore,
    ids: &[uqa_core::DocId],
    control: &StorageReadControl,
) -> StorageBackendResult<RetainedDocumentPage> {
    control.check()?;
    if ids.is_empty() {
        return Ok(BudgetedVec::new(control.memory()));
    }
    let page = source.get_stored_many_controlled(ids, control)?;
    control.check()?;
    if !page.budget().shares_allowance(control.memory()) || page.len() != ids.len() {
        return Err(StorageBackendError::Other(
            "document read page has a foreign allowance or unexpected length".into(),
        ));
    }
    for row in page.iter().flatten() {
        control.check()?;
        if !row.retained_fields().shares_allowance(control) {
            return Err(StorageBackendError::Other(
                "document read payload belongs to a different allowance".into(),
            ));
        }
    }
    control.check()?;
    Ok(page)
}

pub(super) fn copy_fields<'a>(
    source: impl IntoIterator<Item = (&'a str, &'a Value)>,
    control: &StorageReadControl,
) -> StorageBackendResult<RetainedDocumentFields> {
    // Declaration order keeps copied fields alive only while their payload lease is held.
    let mut memory = control.memory().empty_reservation();
    let mut fields = Document::new();
    for (name, value) in source {
        control.check()?;
        memory.grow(size_of::<(String, Value)>())?;
        let mut copied_name = BudgetedString::new(control.memory());
        copied_name.reserve(name.len())?;
        let mut start = 0;
        while start < name.len() {
            control.check()?;
            let mut end = start.saturating_add(4096).min(name.len());
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            copied_name.push_str(&name[start..end])?;
            start = end;
        }
        let copied = value
            .clone_budgeted(control.memory(), control.cancellation())
            .map_err(super::retained::retention_error)?;
        let (name, name_memory) = copied_name.into_parts();
        let (value, value_memory) = copied.into_parts();
        fields.insert(name, value);
        memory.absorb(name_memory);
        memory.absorb(value_memory);
    }
    control.check()?;
    RetainedDocumentFields::from_budgeted(Budgeted::new(fields, memory), control)
}

#[cfg(test)]
mod tests;
