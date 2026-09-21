//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable declared-column addresses and disjoint dynamic field names.

use uqa_core::memory::BudgetedVec;
use uqa_sql::ast::ColumnDef;
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

pub(super) fn prefix(
    columns: &[ColumnDef],
    field: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    let mut key = BudgetedVec::new(control.memory());
    append(&mut key, columns, field, control)?;
    Ok(key)
}

pub(super) fn append(
    key: &mut BudgetedVec<u8>,
    columns: &[ColumnDef],
    field: &str,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    control.check()?;
    if let Some(column) = columns.iter().find(|column| column.name == field) {
        let identity = column
            .object_id
            .filter(|identity| *identity != [0; 16])
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "indexed field {field:?} has no immutable column identity"
                ))
            })?;
        key.push(0)?;
        key.extend_from_slice(&identity)?;
    } else {
        // Registered dynamic fields have no column incarnation, even when the table has other declared columns. Their length-delimited name is scoped by the immutable relation and cannot alias a declared column address.
        key.push(1)?;
        key.extend_from_slice(&(field.len() as u64).to_be_bytes())?;
        key.extend_from_slice(field.as_bytes())?;
    }
    Ok(())
}
