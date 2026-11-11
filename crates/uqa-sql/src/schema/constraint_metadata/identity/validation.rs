//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint identity validation borrows syntax and admits only its temporary identity nodes.

use super::{ConstraintMetadataError, ConstraintMetadataResult};
use uqa_core::memory::{OwnedSet, ProductionControl};

pub(super) fn check(control: &ProductionControl<'_>) -> ConstraintMetadataResult<()> {
    control.check().map_err(execution)
}

pub(super) fn insert<K: Ord>(
    values: &mut OwnedSet<K>,
    value: K,
    control: &ProductionControl<'_>,
) -> ConstraintMetadataResult<bool> {
    check(control)?;
    match control.budget() {
        Some(memory) => values.insert_budgeted(value, memory).map_err(execution),
        None => Ok(values.insert(value)),
    }
}

pub(super) fn any<T>(
    items: impl IntoIterator<Item = T>,
    control: &ProductionControl<'_>,
    mut predicate: impl FnMut(T) -> bool,
) -> ConstraintMetadataResult<bool> {
    for item in items {
        check(control)?;
        if predicate(item) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn execution(error: impl Into<crate::SQLError>) -> ConstraintMetadataError {
    ConstraintMetadataError::Execution(Box::new(error.into()))
}

#[cfg(test)]
mod tests;
