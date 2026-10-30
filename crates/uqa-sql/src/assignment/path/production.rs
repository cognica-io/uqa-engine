//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Assignment paths borrow indirection and admit their fixed operand descriptors before allocation.

use crate::{
    assignment::targets::{assignment_level_iter, AssignmentLevel},
    ast::{
        ArraySliceBounds, AssignmentBinding, AssignmentPathStep, AssignmentStep, AssignmentTarget,
    },
    expr::composites::{descriptor_with_control, CompositeTypeCatalog},
    ColumnType, SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub fn bind_with_control<E>(
    target: &AssignmentTarget<E>,
    types: &[ColumnType],
    composites: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<AssignmentBinding>, SQLError> {
    control.check()?;
    let mut path = ProductionVec::new(*control);
    let mut next_argument = 1;
    for (level, ty) in assignment_level_iter(&target.indirection).zip(types) {
        control.check()?;
        path.push_copy(match &level {
            AssignmentLevel::Field(field) => {
                let mut base = ty;
                while let ColumnType::Domain { base: inner, .. } = base {
                    control.check()?;
                    base = inner;
                }
                let ColumnType::Composite(reference) = base else {
                    return Err(SQLError::Internal(
                        "field assignment lost its composite type".into(),
                    ));
                };
                let descriptor = descriptor_with_control(composites, reference.oid, control)?;
                let (_, attribute) = descriptor
                    .attribute(field)
                    .ok_or_else(|| SQLError::UnknownColumn((*field).to_owned()))?;
                AssignmentPathStep::Field {
                    type_oid: reference.oid,
                    number: attribute.number,
                }
            }
            AssignmentLevel::Subscripts(steps) => {
                let slice = level.is_slice();
                let mut bounds = ArraySliceBounds::default();
                for (dimension, step) in steps.iter().enumerate() {
                    control.check()?;
                    if let AssignmentStep::Slice { lower, upper } = step {
                        bounds.omit(dimension, lower.is_none(), upper.is_none());
                    }
                }
                let first_argument = next_argument;
                next_argument += steps.len() * if slice { 2 } else { 1 };
                AssignmentPathStep::Subscripts {
                    first_argument,
                    dimensions: steps.len(),
                    slice,
                    bounds,
                }
            }
        })?;
    }
    let (path, memory) = path.finish()?.into_parts();
    Ok(control.finish(
        AssignmentBinding {
            path,
            value_argument: next_argument,
        },
        memory,
    )?)
}
