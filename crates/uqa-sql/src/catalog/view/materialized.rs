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
    let collations = collations.ok_or_else(|| {
        SQLError::Internal("stored materialized view has no durable collation metadata".into())
    })?;
    if columns.len() != types.len() || columns.len() != collations.len() {
        return Err(SQLError::Internal(
            "stored materialized view column metadata is inconsistent".into(),
        ));
    }
    for (ty, oid) in types.iter().zip(collations) {
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
    Ok(RowSchema::with_types(columns.to_vec(), types.to_vec())
        .with_column_collations(collations.iter().copied().map(Some)))
}
