//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog adapters lend registered vector names and dimensions without allocating copies.

use std::collections::BTreeMap;
use uqa_core::{memory::MemoryReservation, FieldName};
use uqa_sql::SQLError;
use uqa_storage::{
    read_control::StorageReadControl,
    vector_index::{VectorIndexSource, VectorIndexes},
    VectorIndex,
};

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

impl VectorDimensions for VectorIndexes {
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
    indexes: &dyn VectorIndexSource,
    control: &StorageReadControl,
) -> Result<VectorIndexes, SQLError> {
    VectorIndexes::capture(indexes, control)
        .map_err(|error| snapshot_error("vector indexes", &error))
}

pub(super) fn copy_field(
    field: &str,
    control: &StorageReadControl,
) -> Result<(FieldName, MemoryReservation), SQLError> {
    uqa_storage::vector_index::RetainedVectorIndexesBuilder::copy_field(field, control)
        .map_err(|error| snapshot_error("vector field name", &error))
}
