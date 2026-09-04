//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Eligibility checks for UPDATE's storage-level patch fast path.

use super::super::{
    dml_storage_error, referrers_to_for_actions, BTreeMap, BTreeSet, Engine, SQLError, Value,
};

pub(super) fn can_patch_update_without_full_row(
    engine: &Engine,
    table: &str,
    updates: &BTreeMap<String, Value>,
) -> Result<bool, SQLError> {
    if engine
        .try_check_constraint_definitions(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| constraint.enforced)
    {
        return Ok(false);
    }
    let update_keys: BTreeSet<&str> = updates.keys().map(String::as_str).collect();
    if engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
        .iter()
        .any(|col| {
            col.not_null
                && col.auto_increment.is_none()
                && matches!(updates.get(&col.name), Some(Value::Null))
        })
    {
        return Ok(false);
    }
    if engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| {
            constraint
                .columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return Ok(false);
    }
    if engine
        .try_foreign_keys(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .filter(|fk| fk.enforced)
        .any(|fk| {
            fk.local_columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return Ok(false);
    }
    if referrers_to_for_actions(engine, table)?
        .iter()
        .any(|(_, fk)| {
            fk.ref_columns
                .iter()
                .any(|column| update_keys.contains(column.as_str()))
        })
    {
        return Ok(false);
    }
    Ok(true)
}

pub(super) fn point_lookup_field_is_unique(
    engine: &Engine,
    table: &str,
    lookup_field: &str,
) -> Result<bool, SQLError> {
    Ok(engine
        .try_key_constraints(table)
        .map_err(|err| dml_storage_error("UPDATE", err))?
        .iter()
        .any(|constraint| constraint.columns.len() == 1 && constraint.columns[0] == lookup_field))
}
