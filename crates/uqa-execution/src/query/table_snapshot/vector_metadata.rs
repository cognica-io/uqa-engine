//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog adapters lend registered vector names and dimensions without allocating copies.

use std::collections::BTreeMap;
use uqa_core::{
    memory::{Budgeted, BudgetedString, MemoryReservation},
    FieldName,
};
use uqa_sql::SQLError;
use uqa_storage::{read_control::StorageReadControl, ReadOnlySnapshot, VectorIndex};

use super::snapshot_error;

/// Borrow each registered vector field and its dimensions from the selected catalog boundary. Implementations visit each field once and stop on the first visitor failure.
pub trait VectorDimensions {
    fn visit<'a>(
        &'a self,
        visitor: &mut dyn FnMut(&'a str, u32) -> Result<(), SQLError>,
    ) -> Result<(), SQLError>;
}

impl VectorDimensions for BTreeMap<FieldName, u32> {
    fn visit<'a>(
        &'a self,
        visitor: &mut dyn FnMut(&'a str, u32) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        for (field, dimensions) in self {
            visitor(field, *dimensions)?;
        }
        Ok(())
    }
}

impl VectorDimensions for BTreeMap<FieldName, Box<dyn VectorIndex>> {
    fn visit<'a>(
        &'a self,
        visitor: &mut dyn FnMut(&'a str, u32) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        for (field, index) in self {
            visitor(field, index.dimensions())?;
        }
        Ok(())
    }
}

/// Retain selected physical indexes and their copied field names under the original allowance. Providers keep their own retained corpus ownership; the returned read-only handles retain name and adapter leases through nested snapshots.
pub fn retain_vector_indexes(
    indexes: &BTreeMap<FieldName, Box<dyn VectorIndex>>,
    control: &StorageReadControl,
) -> Result<BTreeMap<FieldName, Box<dyn VectorIndex>>, SQLError> {
    control.cancellation().check()?;
    let mut retained = BTreeMap::new();
    for (field, index) in indexes {
        let (field, memory) = copy_field(field, control)?;
        let index = index
            .snapshot_with_control(control)
            .map_err(|error| snapshot_error("vector index", &error))?;
        control.cancellation().check()?;
        let index = retain_handle(ReadOnlySnapshot::new(index), memory)?;
        retained.insert(field, index);
    }
    Ok(retained)
}

pub(super) fn copy_field(
    field: &str,
    control: &StorageReadControl,
) -> Result<(FieldName, MemoryReservation), SQLError> {
    control.cancellation().check()?;
    let mut name = BudgetedString::new(control.memory());
    name.reserve(field.len())
        .map_err(|error| snapshot_error("vector field name", &error.into()))?;
    for (offset, character) in field.chars().enumerate() {
        if offset % 1024 == 0 {
            control.cancellation().check()?;
        }
        name.push(character)
            .map_err(|error| snapshot_error("vector field name", &error.into()))?;
    }
    Ok(name.into_parts())
}

pub(super) fn retain_handle<T: VectorIndex + 'static>(
    index: T,
    mut memory: MemoryReservation,
) -> Result<Box<dyn VectorIndex>, SQLError> {
    memory
        .grow(size_of::<ReadOnlySnapshot<T>>())
        .map_err(|error| snapshot_error("vector index handle", &error.into()))?;
    let index = ReadOnlySnapshot::from_budgeted(Budgeted::new(index, memory))
        .map_err(|error| snapshot_error("vector index metadata", &error))?;
    Ok(Box::new(index))
}
