//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed host array construction uses native parameter binding and catalog-aware scalar casts.

use super::{input, SQLParam};
use crate::{expr::EngineHook, ColumnType, FunctionTypeResolver, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    ArrayValue, Value,
};

/// Construct a typed array with explicit SQL bounds and row-major parameters. Every source identity binds before any input callback; conversion and outer-domain validation belong to the caller's statement scope.
#[allow(clippy::too_many_arguments)]
pub fn create_array_parameter_with_control(
    type_name: &str,
    dimensions: &[usize],
    lower_bounds: &[i32],
    elements: &[SQLParam],
    types: &dyn FunctionTypeResolver,
    input: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<SQLParam>, SQLError> {
    let ty = input::resolve_type(type_name, types, control)?;
    let mut base = &*ty;
    while let ColumnType::Domain { base: inner, .. } = base {
        base = inner;
    }
    let element = base.array_element_type().ok_or_else(|| {
        SQLError::TypeMismatch("array construction requires an array type".into())
    })?;
    let sources = input::bind_sources(elements, types, control)?;
    let array = ArrayValue::from_row_major_with_control(
        dimensions,
        lower_bounds,
        elements.len(),
        control,
        |index| {
            input::cast_parameter(
                &elements[index],
                sources[index].as_ref(),
                element,
                input,
                control,
            )
        },
    )?
    .ok_or_else(|| SQLError::Routine {
        sqlstate: "22023".into(),
        message: "invalid array dimensions, bounds or element count".into(),
    })?;
    let (array, memory) = array.into_parts();
    let array = array.with_element_type_oid(Some(
        crate::catalog::type_metadata::pg_type_oid(element) as u32,
    ));
    let value = control.finish(Value::Array(array), memory)?;
    let value = if matches!(*ty, ColumnType::Domain { .. }) {
        let source = base.catalog_name_with_control(control)?;
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
    input::finish(value, ty, input, control)
}
