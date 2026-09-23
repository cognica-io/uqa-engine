//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected field copies admit their output while the fixed source remains borrowed.

use super::{read_field_presence, DocumentStore};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::{memory::Budgeted, DocId, Value};

/// Copy one field from an already selected, immutable view. Field metadata distinguishes absence from a stored NULL without materializing unrelated fields. The returned payload keeps its production allowance until the consumer releases or transfers it.
pub fn read_selected_field(
    source: &(impl DocumentStore + ?Sized),
    id: DocId,
    field: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<Value>>> {
    control.check()?;
    let mut result = None;
    let mut visited = false;
    let mut repeated = false;
    let read = source.with_field_ref_controlled(id, field, control, &mut |value| {
        if visited {
            repeated = true;
            return Err(StorageBackendError::Other(
                "selected field read repeated its value".into(),
            ));
        }
        visited = true;
        result = Some(control.check().and_then(|()| {
            value
                .map(|value| {
                    value
                        .clone_budgeted(control.memory(), control.cancellation())
                        .map_err(super::retained::retention_error)
                })
                .transpose()
        }));
        Ok(())
    });
    // Preserve a copy failure if a provider observes cancellation while returning from the visitor.
    let result = result.transpose()?;
    if repeated {
        return Err(StorageBackendError::Other(
            "selected field read repeated its value".into(),
        ));
    }
    read?;
    control.check()?;
    result.ok_or_else(|| StorageBackendError::Other("selected field read omitted its value".into()))
}

pub(super) fn visit_selected_field(
    source: &(impl DocumentStore + ?Sized),
    id: DocId,
    field: &str,
    control: &StorageReadControl,
    visitor: &mut dyn FnMut(Option<&Value>) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    let present = read_field_presence(source, &[id], &[field], control)?;
    if !present[0] {
        visitor(None)?;
        return control.check();
    }
    drop(present);
    let mut result = None;
    let mut visited = false;
    let read = source.for_each_fields_multi_ref_with_presence(
        &[id],
        &[field],
        &mut |actual, exists, values| {
            if visited || actual != id || !exists || values.len() != 1 {
                result = Some(Err(StorageBackendError::Other(
                    "selected field read returned an inconsistent row".into(),
                )));
                return false;
            }
            visited = true;
            result = Some(control.check().and_then(|()| visitor(Some(values[0]))));
            false
        },
    );
    // Preserve a copy failure if a provider observes cancellation while returning from the visitor.
    let result = result.transpose()?;
    read?;
    control.check()?;
    result.ok_or_else(|| StorageBackendError::Other("selected field read omitted its row".into()))
}

#[cfg(test)]
mod tests;
