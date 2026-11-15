//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured declarations preserve creation rules without executing defaults or compiling source bodies.

use super::{parameters::ParameterLayout, validation, RoutineTypeCatalog};
use crate::{
    ast::{ColumnType, CreateFunction, FunctionBody},
    SQLError,
};
use uqa_core::memory::ProductionControl;

pub fn validate_captured_declaration(
    definition: &CreateFunction,
    types: &dyn RoutineTypeCatalog,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    validate_language(definition)?;
    let mut layout = ParameterLayout::default();
    for (index, parameter) in definition.params.iter().enumerate() {
        layout.visit(
            &definition.params[..index],
            parameter,
            definition.is_procedure,
            || variadic_array(types, &parameter.type_name, control),
            control,
        )?;
    }
    validation::validate_with_control(definition, control)
}

pub(in crate::routines) fn validate_language(definition: &CreateFunction) -> Result<(), SQLError> {
    if !matches!(definition.language.as_str(), "plpgsql" | "sql") {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("language \"{}\" does not exist", definition.language),
        });
    }
    if definition.language == "plpgsql" && matches!(definition.body, FunctionBody::Statements(_)) {
        return Err(super::routine_definition_error(
            "inline SQL function body only valid for language SQL",
        ));
    }
    Ok(())
}

pub(super) fn variadic_array(
    types: &dyn RoutineTypeCatalog,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let canonical =
        crate::type_resolution::canonical_routine_type_name_with_control(name, control)?;
    if canonical.ends_with("[]")
        || matches!(
            canonical.as_str(),
            "any" | "anyarray" | "anycompatiblearray" | "int2vector" | "oidvector"
        )
    {
        return Ok(true);
    }
    let Some(ty) = types.resolve_catalog_column_type_with_control(&canonical, control)? else {
        return Ok(false);
    };
    Ok(matches!(&*ty, ColumnType::Array(_) | ColumnType::AnyArray))
}

#[cfg(test)]
mod tests;
