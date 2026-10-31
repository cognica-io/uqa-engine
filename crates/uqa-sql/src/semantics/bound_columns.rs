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
    bound_source_schema_with_control(
        schema,
        bound,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub fn bound_source_schema_with_control(
    schema: &RowSchema,
    bound: Option<&[String]>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    control.check()?;
    let Some(bound) = bound else {
        return Ok(schema.clone());
    };
    for name in bound {
        control.check()?;
        if !schema.columns().contains(name) {
            return Err(SQLError::UnknownColumn(
                control.copy_text(name)?.into_parts().0,
            ));
        }
    }
    RowSchema::select_with_control(
        schema,
        bound.iter().map(|name| (name.as_str(), name.as_str())),
        control,
    )
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
