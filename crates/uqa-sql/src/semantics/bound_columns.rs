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
    bound_source_column_names_with_control(
        current.iter().map(String::as_str),
        Some(bound),
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .map_err(|_| unreachable!("ordinary source column selection"))
}

/// Select a stored declaration's bound columns before copying its public labels.
pub fn bound_source_column_names_with_control<'a>(
    current: impl IntoIterator<Item = &'a str>,
    bound: Option<&[String]>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<Vec<String>>, SQLError> {
    use uqa_core::memory::ProductionVec;
    let mut output = ProductionVec::new(*control);
    let Some(bound) = bound else {
        for name in current {
            output.push_produced(control.copy_text(name)?)?;
        }
        return Ok(output.finish()?);
    };
    let mut declared = ProductionVec::new(*control);
    for name in current {
        declared.push_copy(name)?;
    }
    for name in bound {
        control.check()?;
        if !declared.contains(&name.as_str()) {
            return Err(SQLError::UnknownColumn(name.clone()));
        }
    }
    for name in bound {
        output.push_produced(control.copy_text(name)?)?;
    }
    Ok(output.finish()?)
}
