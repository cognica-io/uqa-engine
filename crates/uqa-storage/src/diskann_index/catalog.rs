//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared catalog validation; providers own physical row addressing and captured identity guards.

use crate::read_control::StorageReadControl;
use crate::vector_index::DiskANNIndexParams;
use crate::{StorageBackendError, StorageBackendResult, VectorFieldSchema};

mod identity;
pub use identity::{resolve_scope, DiskANNIndexResolver, DiskANNIndexScope};

pub fn validate_field(
    fields: &[VectorFieldSchema],
    field: &str,
    dimensions: u32,
) -> StorageBackendResult<()> {
    let mut matching = fields.iter().filter(|item| item.field == field);
    if matching
        .next()
        .is_none_or(|item| item.dimensions != dimensions)
        || matching.next().is_some()
        || dimensions == 0
    {
        return Err(invalid("canonical field has no unique matching dimensions"));
    }
    Ok(())
}

pub fn parameters(
    method: &str,
    columns_json: &str,
    parameters_json: &str,
    field: &str,
    dimensions: u32,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNIndexParams> {
    control.check()?;
    let bytes = columns_json
        .len()
        .checked_add(parameters_json.len())
        .and_then(|size| size.checked_mul(16))
        .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
    let _memory = control.memory().reserve(bytes)?;
    let columns: Vec<String> =
        serde_json::from_str(columns_json).map_err(|error| invalid(&error.to_string()))?;
    if !method.eq_ignore_ascii_case("diskann") || columns.as_slice() != [field] {
        return Err(invalid("catalog index does not own this canonical field"));
    }
    let parameters =
        serde_json::from_str(parameters_json).map_err(|error| invalid(&error.to_string()))?;
    let result = DiskANNIndexParams::from_catalog_map(dimensions, &parameters)?;
    control.check()?;
    Ok(result)
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid DiskANN catalog binding: {message}"))
}
