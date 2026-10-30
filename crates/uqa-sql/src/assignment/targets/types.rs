//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Assignment type traversal borrows indirection and admits each selected declaration before copying it.

use super::{error, level_iter, AssignmentLevel, AssignmentStep, AssignmentTarget};
use crate::{
    expr::composites::{descriptor_with_control, CompositeTypeCatalog},
    plan::copy::SyntaxProduction,
    production::Buffer,
    ColumnType, SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

/// Retain the type at every assignment level under the caller's original allowance.
pub fn field_assignment_types_with_control<E>(
    target: &AssignmentTarget<E>,
    declared: &ColumnType,
    composites: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ColumnType>>, SQLError> {
    let mut types = Buffer::new(control);
    types.push(declared.clone_with_control(control)?, control)?;
    for level in level_iter(&target.indirection) {
        let next = next_type(
            target,
            types.last().expect("declared type"),
            level,
            composites,
            control,
        )?;
        types.push(next, control)?;
    }
    let output = SyntaxProduction::new(*control);
    let mut values = output.vector(types.len())?;
    for value in types {
        values.push(output.take(value))?;
    }
    let values = values.finish()?;
    output.finish(values)
}

/// Select only the value type, retiring each intermediate declaration as the traversal advances.
pub fn assignment_input_type_with_control<E>(
    target: &AssignmentTarget<E>,
    declared: &ColumnType,
    composites: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    let mut current = declared.clone_with_control(control)?;
    for level in level_iter(&target.indirection) {
        current = next_type(target, &current, level, composites, control)?;
    }
    Ok(current)
}

fn next_type<E>(
    target: &AssignmentTarget<E>,
    current: &ColumnType,
    level: AssignmentLevel<'_, E>,
    composites: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    control.check()?;
    let mut base = current;
    while let ColumnType::Domain { base: inner, .. } = base {
        base = inner;
    }
    match level {
        AssignmentLevel::Field(field) => {
            let ColumnType::Composite(reference) = base else {
                let name = current.display_name_with_control(control)?;
                return Err(error("42804", format!(
                    "cannot assign to field \"{field}\" of column \"{}\" because its type {} is not a composite type",
                    target.column, &*name
                )));
            };
            let descriptor = descriptor_with_control(composites, reference.oid, control)?;
            let Some((_, attribute)) = descriptor.attribute(field) else {
                let name = current.display_name_with_control(control)?;
                return Err(error("42703", format!(
                    "cannot assign to field \"{field}\" of column \"{}\" because there is no such column in data type {}",
                    target.column, &*name
                )));
            };
            Ok(attribute.ty.clone_with_control(control)?)
        }
        AssignmentLevel::Subscripts(steps) => {
            let element = match base {
                ColumnType::Array(element) => &**element,
                ColumnType::Int2Vector => &ColumnType::SmallInteger,
                ColumnType::OidVector => &ColumnType::Oid,
                _ => {
                    let name = current.display_name_with_control(control)?;
                    return Err(error(
                        "42804",
                        format!(
                            "cannot subscript type {} because it does not support subscripting",
                            &*name
                        ),
                    ));
                }
            };
            if steps.len() > 6 {
                return Err(error(
                    "54000",
                    format!(
                        "number of array dimensions ({}) exceeds the maximum allowed (6)",
                        steps.len()
                    ),
                ));
            }
            if steps
                .iter()
                .any(|step| matches!(step, AssignmentStep::Slice { .. }))
            {
                let output = SyntaxProduction::new(*control);
                let element = output.copy(element)?;
                let array = ColumnType::Array(output.boxed(element)?);
                output.finish(array)
            } else {
                let mut element = element;
                while let ColumnType::Array(inner) = element {
                    element = inner;
                }
                Ok(element.clone_with_control(control)?)
            }
        }
    }
}
