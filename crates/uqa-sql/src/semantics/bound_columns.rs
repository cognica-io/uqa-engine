//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent column binding for source schemas.

use crate::{RowSchema, SQLError};

pub fn bound_source_schema(
    schema: &RowSchema,
    bound: Option<&[String]>,
) -> Result<RowSchema, SQLError> {
    let Some(bound) = bound else {
        return Ok(schema.clone());
    };
    let columns = bound_source_column_names(schema.columns().to_vec(), Some(bound))?;
    Ok(RowSchema::select(
        schema,
        &columns
            .into_iter()
            .map(|name| (name.clone(), name))
            .collect::<Vec<_>>(),
    ))
}

pub fn bound_source_column_names(
    current: Vec<String>,
    bound: Option<&[String]>,
) -> Result<Vec<String>, SQLError> {
    let Some(bound) = bound else {
        return Ok(current);
    };
    for name in bound {
        if !current.contains(name) {
            return Err(SQLError::UnknownColumn(name.clone()));
        }
    }
    Ok(bound.to_vec())
}
