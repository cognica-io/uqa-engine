//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Procedure output descriptions admit selected names and types before publishing their independently owned row schema.

use crate::{
    ast::{CreateFunction, FunctionParamMode},
    routines::declaration::RoutineTypeCatalog,
    ColumnType, RowSchema, SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionVec};

pub fn call_output_schema_with_control(
    catalog: &dyn RoutineTypeCatalog,
    definition: &CreateFunction,
    parameter_types: &[String],
    control: &ProductionControl<'_>,
) -> Result<Option<RowSchema>, SQLError> {
    control.check()?;
    let outputs = || {
        definition
            .params
            .iter()
            .enumerate()
            .filter(|(_, parameter)| {
                matches!(
                    parameter.mode,
                    FunctionParamMode::Out | FunctionParamMode::InOut | FunctionParamMode::Table
                )
            })
    };
    let count = outputs().count();
    if count == 0 {
        return Ok(None);
    }
    let mut names = ProductionVec::new(*control);
    names.reserve(count)?;
    for (output, (_, parameter)) in outputs().enumerate() {
        names.push_produced(if parameter.name.is_empty() {
            control.format(format_args!("column{}", output + 1))?
        } else {
            control.copy_text(&parameter.name)?
        })?;
    }
    let names = names.finish()?;
    let mut types = ProductionVec::new(*control);
    types.reserve(count)?;
    for (index, _) in outputs() {
        let name = &parameter_types[index];
        let ty = match catalog.resolve_catalog_column_type_with_control(name, control)? {
            Some(ty) => Some(ty),
            None => match ColumnType::from_sql_name_with_control(name, control) {
                Ok(ty) => Some(ty),
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Err(error)
                }
                Err(_) => None,
            },
        }
        .ok_or_else(|| {
            control
                .format(format_args!("unknown type `{name}`"))
                .map_or_else(Into::into, |message| {
                    SQLError::TypeMismatch(message.into_parts().0)
                })
        })?;
        types.push_produced(ty)?;
    }
    let types = types.finish()?;
    Ok(Some(RowSchema::with_column_metadata_with_control(
        None,
        names
            .iter()
            .zip(types.iter())
            .map(|(name, ty)| (name.as_str(), Some(ty), None)),
        control,
    )?))
}
