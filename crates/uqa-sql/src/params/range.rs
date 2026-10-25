//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed range parameters share SQL endpoint coercion and the selected native constructor.

use super::{input, SQLParam};
use crate::{
    expr::{EngineHook, EngineValueCatalog, RangeBoundsInput, RangeValueDescriptor},
    ColumnType, FunctionTypeResolver, SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

#[cfg(test)]
mod tests;

/// Host range bounds use absent endpoints for infinity and an explicit variant for the empty set.
pub enum RangeParameterBounds<'a> {
    Empty,
    Bounds {
        lower: Option<&'a SQLParam>,
        upper: Option<&'a SQLParam>,
        lower_inclusive: bool,
        upper_inclusive: bool,
    },
}

/// Bind both source identities before endpoint input; conversion, selected canonical support and the outer domain execute in the caller's statement scope.
pub fn create_range_parameter_with_control(
    type_name: &str,
    bounds: RangeParameterBounds<'_>,
    types: &dyn FunctionTypeResolver,
    input: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<SQLParam>, SQLError> {
    let ty = input::resolve_type(type_name, types, control)?;
    let base = base_type(&ty);
    if !matches!(base, ColumnType::Range(_) | ColumnType::DeclaredRange(_)) {
        return Err(SQLError::TypeMismatch(
            "range construction requires a range type".into(),
        ));
    }
    let catalog = EngineValueCatalog(input);
    let descriptor =
        RangeValueDescriptor::from_type_with_control(base, Some(&catalog), control)?
            .ok_or_else(|| SQLError::Internal("range construction has no descriptor".into()))?;
    let bounds = match bounds {
        RangeParameterBounds::Empty => RangeBoundsInput::Empty,
        RangeParameterBounds::Bounds {
            lower,
            upper,
            lower_inclusive,
            upper_inclusive,
        } => {
            let source = |parameter: Option<&SQLParam>| {
                parameter
                    .map(|parameter| {
                        super::bind_parameter_type_with_control(parameter, types, control)
                    })
                    .transpose()
                    .map(Option::flatten)
            };
            let lower_source = source(lower)?;
            let upper_source = source(upper)?;
            let bound = |parameter: Option<&SQLParam>, source: Option<&ColumnType>| {
                let Some(parameter) = parameter else {
                    return Ok(control.finish(None, control.empty_reservation())?);
                };
                let (value, memory) =
                    input::cast_parameter(parameter, source, &descriptor.endpoint, input, control)?
                        .into_parts();
                Ok::<_, SQLError>(control.finish(
                    if matches!(value, Value::Null) {
                        None
                    } else {
                        Some(value)
                    },
                    memory,
                )?)
            };
            RangeBoundsInput::Bounds {
                lower: bound(lower, lower_source.as_deref())?,
                upper: bound(upper, upper_source.as_deref())?,
                lower_inclusive,
                upper_inclusive,
            }
        }
    };
    let value = match base {
        ColumnType::Range(subtype) => {
            crate::expr::range_value_from_bounds_with_control(*subtype, bounds, control)?
        }
        ColumnType::DeclaredRange(reference) => {
            input.construct_range(reference.oid, bounds, control)?
        }
        _ => unreachable!("checked range declaration"),
    };
    finish(value, ty, input, control)
}

/// Normalize copied, typed members without re-input of admitted catalog values. A NULL member is an error; an empty slice constructs a present empty multirange.
pub fn create_multirange_parameter_with_control(
    type_name: &str,
    members: &[SQLParam],
    types: &dyn FunctionTypeResolver,
    input: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<SQLParam>, SQLError> {
    let ty = input::resolve_type(type_name, types, control)?;
    let base = base_type(&ty);
    if !matches!(
        base,
        ColumnType::Multirange(_) | ColumnType::DeclaredMultirange(_)
    ) {
        return Err(SQLError::TypeMismatch(
            "multirange construction requires a multirange type".into(),
        ));
    }
    let catalog = EngineValueCatalog(input);
    let descriptor = RangeValueDescriptor::from_type_with_control(base, Some(&catalog), control)?
        .ok_or_else(|| {
        SQLError::Internal("multirange construction has no descriptor".into())
    })?;
    let range_type = descriptor.range_type_with_control(Some(&catalog), control)?;
    let sources = input::bind_sources(members, types, control)?;
    let mut values = ProductionVec::new(*control);
    values.reserve(members.len())?;
    for (member, source) in members.iter().zip(sources.iter()) {
        let value = input::cast_parameter(member, source.as_ref(), &range_type, input, control)?;
        if matches!(*value, Value::Null) {
            return Err(SQLError::Routine {
                sqlstate: "22004".into(),
                message: "multirange values cannot contain null members".into(),
            });
        }
        values.push_produced(value)?;
    }
    let value = match base {
        ColumnType::Multirange(subtype) => {
            crate::expr::multirange_value_from_members_with_control(*subtype, &values, control)?
        }
        ColumnType::DeclaredMultirange(reference) => {
            input.construct_multirange(reference.oid, &values, control)?
        }
        _ => unreachable!("checked multirange declaration"),
    };
    finish(value, ty, input, control)
}

fn base_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    ty
}

fn finish(
    value: Produced<Value>,
    ty: Produced<ColumnType>,
    input: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<SQLParam>, SQLError> {
    let value = if matches!(*ty, ColumnType::Domain { .. }) {
        let source = base_type(&ty).catalog_name_with_control(control)?;
        let target = ty.catalog_name_with_control(control)?;
        crate::expr::cast_value_with_type_resolution_with_control(
            &value,
            Some(&source),
            &target,
            Some(input),
            control,
        )?
    } else {
        value
    };
    input::finish(value, ty, control)
}
