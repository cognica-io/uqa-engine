//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field-existence metadata preserves explicit NULL without transferring row values.

use uqa_core::{memory::BudgetedVec, DocId};

use super::{read_stored_documents, DocumentStore};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

#[cfg(test)]
mod tests;

/// Validate one field-presence page while its producer reservation stays live.
pub fn read_field_presence(
    source: &(impl DocumentStore + ?Sized),
    ids: &[DocId],
    fields: &[&str],
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<bool>> {
    control.check()?;
    if ids.is_empty() || fields.is_empty() {
        return Ok(BudgetedVec::new(control.memory()));
    }
    let expected = ids.len().checked_mul(fields.len()).ok_or_else(|| {
        StorageBackendError::Other("document field-presence count overflow".into())
    })?;
    let page = source.field_presence_controlled(ids, fields, control)?;
    control.check()?;
    if !page.budget().shares_allowance(control.memory()) || page.len() != expected {
        return Err(StorageBackendError::Other(
            "document field-presence page has a foreign allowance or unexpected length".into(),
        ));
    }
    Ok(page)
}

pub(super) fn from_controlled_rows(
    source: &(impl DocumentStore + ?Sized),
    ids: &[DocId],
    fields: &[&str],
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<bool>> {
    control.check()?;
    let mut present = BudgetedVec::new(control.memory());
    if ids.is_empty() || fields.is_empty() {
        return Ok(present);
    }
    let rows = read_stored_documents(source, ids, control)?;
    for row in rows.iter() {
        for field in fields {
            control.check()?;
            present.push(
                row.as_ref()
                    .is_some_and(|row| row.fields().contains_key(*field)),
            )?;
        }
    }
    control.check()?;
    Ok(present)
}
