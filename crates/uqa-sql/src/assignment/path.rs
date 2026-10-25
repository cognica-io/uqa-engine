//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluate each partial-write operand once, then rebuild its typed containers.

use super::{containers, targets};
use crate::{
    ast::{
        ArraySliceBounds, AssignmentBinding, AssignmentPathStep, AssignmentStep, AssignmentTarget,
    },
    expr::{composites::CompositeTypeCatalog, SQLValueCatalog},
    ColumnType, SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

#[cfg(test)]
mod physical_tests;
#[cfg(test)]
mod tests;

/// Bind field identities and operand positions independently of their scalar representation.
pub fn bind<E>(
    target: &AssignmentTarget<E>,
    types: &[ColumnType],
    composites: Option<&dyn CompositeTypeCatalog>,
) -> Result<AssignmentBinding, SQLError> {
    let levels = targets::assignment_levels(&target.indirection);
    let mut path = Vec::with_capacity(levels.len());
    let mut next_argument = 1;
    for (level, ty) in levels.iter().zip(types) {
        path.push(match level {
            targets::AssignmentLevel::Field(field) => {
                let mut base = ty;
                while let ColumnType::Domain { base: inner, .. } = base {
                    base = inner;
                }
                let ColumnType::Composite(reference) = base else {
                    return Err(SQLError::Internal(
                        "field assignment lost its composite type".into(),
                    ));
                };
                let descriptor = crate::expr::composites::descriptor(composites, reference.oid)?;
                let (_, attribute) = descriptor
                    .attribute(field)
                    .ok_or_else(|| SQLError::UnknownColumn((*field).to_owned()))?;
                AssignmentPathStep::Field {
                    type_oid: reference.oid,
                    number: attribute.number,
                }
            }
            targets::AssignmentLevel::Subscripts(steps) => {
                let slice = level.is_slice();
                let mut bounds = ArraySliceBounds::default();
                for (dimension, step) in steps.iter().enumerate() {
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
        });
    }
    Ok(AssignmentBinding {
        path,
        value_argument: next_argument,
    })
}

/// Evaluated subscript groups use `PostgreSQL`'s fixed maximum dimensionality.
#[derive(Clone, Copy)]
pub enum AssignmentContainer {
    Field {
        type_oid: u32,
        number: i16,
    },
    Subscripts {
        bounds: [(Option<i32>, Option<i32>); crate::ast::MAX_ARRAY_SUBSCRIPT_DIMENSIONS],
        dimensions: usize,
        slice: bool,
    },
}

impl AssignmentContainer {
    pub fn subscripts(
        bounds: &[(Option<i32>, Option<i32>)],
        slice: bool,
    ) -> Result<Self, SQLError> {
        let mut stored = [(None, None); crate::ast::MAX_ARRAY_SUBSCRIPT_DIMENSIONS];
        if bounds.len() > stored.len() {
            return Err(containers::array_error(
                uqa_core::ArrayAssignmentError::SubscriptCount,
            ));
        }
        stored[..bounds.len()].copy_from_slice(bounds);
        Ok(Self::Subscripts {
            bounds: stored,
            dimensions: bounds.len(),
            slice,
        })
    }
}

pub fn evaluate<B: std::ops::Deref<Target = Value>>(
    binding: &AssignmentBinding,
    arguments: usize,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    mut evaluate: impl FnMut(usize) -> Result<B, SQLError>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    if binding.value_argument.checked_add(1) != Some(arguments) {
        return Err(SQLError::Internal(
            "assignment operand layout changed".into(),
        ));
    }
    let current = evaluate(0)?;
    let mut path = ProductionVec::new(*control);
    for step in &binding.path {
        control.check()?;
        path.push_copy(match *step {
            AssignmentPathStep::Field { type_oid, number } => {
                AssignmentContainer::Field { type_oid, number }
            }
            AssignmentPathStep::Subscripts {
                first_argument,
                dimensions,
                slice,
                bounds: omitted,
            } => {
                let width = if slice { 2 } else { 1 };
                if first_argument == 0
                    || first_argument
                        .checked_add(dimensions.saturating_mul(width))
                        .is_none_or(|end| end > binding.value_argument)
                {
                    return Err(SQLError::Internal(
                        "assignment subscript operands changed".into(),
                    ));
                }
                let access = crate::ast::ArrayAccess::from_dispatch(
                    if slice {
                        crate::ast::FunctionDispatch::ArraySlicesWithBounds(omitted)
                    } else {
                        crate::ast::FunctionDispatch::ArraySubscripts
                    },
                    1 + dimensions * width,
                    |_| false,
                )
                .expect("array assignment access")?;
                access.validate_dimensions()?;
                let bounds = containers::evaluate_subscript_bounds(
                    dimensions,
                    slice,
                    control,
                    |dimension, upper| {
                        let offset = dimension * width + if slice { usize::from(upper) } else { 0 };
                        if access.is_omitted(1 + offset) {
                            return Ok(None);
                        }
                        let bound = evaluate(first_argument + offset)?;
                        let bound = crate::expr::cast_value_from_with_control(
                            &bound, "integer", None, control,
                        )?;
                        match *bound {
                            Value::Int(value) => {
                                i32::try_from(value)
                                    .map(Some)
                                    .map_err(|_| SQLError::Routine {
                                        sqlstate: "22003".into(),
                                        message: "integer out of range".into(),
                                    })
                            }
                            Value::Null => Err(SQLError::Routine {
                                sqlstate: "22004".into(),
                                message: "array subscript in assignment must not be null".into(),
                            }),
                            _ => Err(SQLError::Internal(
                                "array assignment bound is not an integer".into(),
                            )),
                        }
                    },
                )?;
                AssignmentContainer::subscripts(&bounds, slice)?
            }
        })?;
    }
    let value = evaluate(binding.value_argument)?;
    assign_with_control(&current, &path, &value, catalog, control)
}

/// Walk once down the path and rebuild in reverse; retained parents and scratch frames stay admitted until completion or error.
pub fn assign_with_control(
    current: &Value,
    path: &[AssignmentContainer],
    value: &Value,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    struct Frame {
        current: Value,
        step: AssignmentContainer,
    }
    control.check()?;
    let mut frames = ProductionVec::new(*control);
    let mut current = control.copy_value(current)?;
    for (position, step) in path.iter().enumerate() {
        control.check()?;
        if matches!(step, AssignmentContainer::Subscripts { dimensions, .. } if *dimensions > crate::ast::MAX_ARRAY_SUBSCRIPT_DIMENSIONS)
        {
            return Err(containers::array_error(
                uqa_core::ArrayAssignmentError::SubscriptCount,
            ));
        }
        if let Value::Datum(datum) = &*current {
            current =
                crate::expr::datums::read_with_value_catalog_and_control(datum, catalog, control)?;
        }
        let child = if position + 1 == path.len() {
            control.copy_value(&Value::Null)?
        } else {
            read_child(&current, *step, catalog, control)?
        };
        let (parent, memory) = current.into_parts();
        frames.push_produced(control.finish(
            Frame {
                current: parent,
                step: *step,
            },
            memory,
        )?)?;
        current = child;
    }
    let mut result = control.copy_value(value)?;
    let frames = frames.finish()?;
    for frame in frames.iter().rev() {
        control.check()?;
        result = match frame.step {
            AssignmentContainer::Field { type_oid, number } => containers::field_store(
                type_oid,
                number,
                &frame.current,
                &result,
                catalog,
                control,
            )?,
            AssignmentContainer::Subscripts {
                bounds,
                dimensions,
                slice,
            } => containers::assign_array_with_control(
                &frame.current,
                &result,
                &bounds[..dimensions],
                slice,
                catalog,
                control,
            )?,
        };
    }
    Ok(result)
}

fn read_child(
    current: &Value,
    step: AssignmentContainer,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let value = match step {
        AssignmentContainer::Field { type_oid, number } => {
            let types = crate::expr::ValueCompositeCatalog(catalog);
            let descriptor = crate::expr::composites::descriptor(Some(&types), type_oid)?;
            let attribute = descriptor
                .attributes
                .iter()
                .find(|attribute| attribute.number == number)
                .ok_or_else(|| SQLError::Internal("assignment field disappeared".into()))?;
            return Ok(control.copy_value(match current {
                Value::Null => &Value::Null,
                Value::Record(record) => record
                    .iter()
                    .find(|(name, _)| *name == &attribute.name)
                    .map_or(&Value::Null, |(_, value)| value),
                _ => {
                    return Err(SQLError::Internal(
                        "field assignment has a non-composite container".into(),
                    ))
                }
            })?);
        }
        AssignmentContainer::Subscripts {
            bounds,
            dimensions,
            slice,
        } => {
            if slice {
                return Err(SQLError::Internal(
                    "assignment slice cannot have a child field".into(),
                ));
            }
            let mut indices = [0; crate::ast::MAX_ARRAY_SUBSCRIPT_DIMENSIONS];
            for (index, (_, upper)) in indices.iter_mut().zip(&bounds[..dimensions]) {
                *index = upper
                    .ok_or_else(|| SQLError::Internal("assignment index lost its bound".into()))?;
            }
            match current {
                Value::Null => &Value::Null,
                Value::Array(array) => array
                    .element_at(&indices[..dimensions])
                    .unwrap_or(&Value::Null),
                _ => {
                    return Err(SQLError::Internal(
                        "array assignment has a nonarray container".into(),
                    ))
                }
            }
        }
    };
    Ok(control.copy_value(value)?)
}
