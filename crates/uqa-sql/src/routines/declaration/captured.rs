//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured declarations preserve creation rules without executing defaults or compiling source bodies.

use super::{parameters::ParameterLayout, validation, RoutineTypeCatalog};
use crate::{
    ast::{ColumnType, CreateFunction},
    routines::{attributes, registration},
    SQLError,
};
use uqa_core::memory::ProductionControl;

pub fn validate_captured_declaration(
    definition: &CreateFunction,
    types: &dyn RoutineTypeCatalog,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    attributes::validate_cost(definition.cost)?;
    attributes::validate_rows(definition.rows)?;
    if let Some(support) = definition.support.as_deref() {
        registration::validate_routine_support_identity(support)?;
    }
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
    attributes::validate_rows_applicability(definition.rows, definition.returns_set())?;
    validation::validate_with_control(definition, control)
}

pub(in crate::routines) fn validate_language(definition: &CreateFunction) -> Result<(), SQLError> {
    attributes::validate_routine_language(definition)?;
    attributes::validate_body_form(definition, &definition.attribute_clauses)
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
