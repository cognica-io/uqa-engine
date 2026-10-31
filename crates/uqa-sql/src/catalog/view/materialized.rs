//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialized relation columns keep their creation-time collation independently of query execution and refresh.

use crate::{ColumnType, RowSchema, SQLError};

pub fn materialized_view_schema(
    columns: &[String],
    types: &[Option<ColumnType>],
    collations: Option<&[u32]>,
) -> Result<RowSchema, SQLError> {
    super::materialized_view_schema_with_control(
        columns,
        types,
        collations,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub(super) fn validate_metadata<'a>(
    columns: &[String],
    types: &[Option<ColumnType>],
    collations: Option<&'a [u32]>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<&'a [u32], SQLError> {
    let collations = collations.ok_or_else(|| {
        SQLError::Internal("stored materialized view has no durable collation metadata".into())
    })?;
    if columns.len() != types.len() || columns.len() != collations.len() {
        return Err(SQLError::Internal(
            "stored materialized view column metadata is inconsistent".into(),
        ));
    }
    for (ty, oid) in types.iter().zip(collations) {
        control.check()?;
        if ty
            .as_ref()
            .is_some_and(crate::schema::composites::type_is_collatable)
            != (*oid != 0)
        {
            return Err(SQLError::Internal(
                "stored materialized view collation does not match its column type".into(),
            ));
        }
    }
    Ok(collations)
}
