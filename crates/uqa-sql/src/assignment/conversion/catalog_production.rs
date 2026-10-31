//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog assignment retains each input while its selected conversion produces an admitted replacement.

use super::{AssignmentContext, ColumnType, SQLError, Value};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue,
};

pub fn coerce_assignment_value_with_control(
    context: &dyn AssignmentContext,
    value: Produced<Value>,
    target: &ColumnType,
    source: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let (value, memory) = value.into_parts();
    let value = control.finish(value, memory)?;
    if super::physical_identity(&value, source, target)
        || source.is_some_and(|source| super::same_catalog_value_type(source, target))
    {
        return super::normalize_legacy_vector_carrier_with_control(&value, target, control)
            .map(|replacement| replacement.unwrap_or(value));
    }
    if matches!(target, ColumnType::Domain { .. }) {
        let source = source
            .map(|source| source.catalog_name_with_control(control))
            .transpose()?;
        if let Some(value) = crate::assignment::domain::assign_domain_value_with_control(
            context,
            &value,
            source.as_deref().map(String::as_str),
            target,
            control,
        )? {
            return Ok(value);
        }
    }
    let text = if target.is_character_string() {
        source
            .map(|source| {
                crate::expr::format_regtype_value_with_control(
                    &value,
                    source,
                    Some(context),
                    control,
                )
            })
            .transpose()?
            .flatten()
    } else {
        None
    };
    let value = match text {
        Some(text) => {
            let (text, memory) = text.into_parts();
            control.finish(Value::Str(text), memory)?
        }
        None => value,
    };
    convert_with_catalog(context, value, target, source, control)
}

pub(super) fn convert_with_catalog(
    context: &dyn AssignmentContext,
    value: Produced<Value>,
    target: &ColumnType,
    source: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let value = if super::textual_target(target)
        && crate::expr::enums::contains_enum_carrier(&value)
    {
        crate::expr::enums::render_enum_labels_with_control(context.enum_labels(), &value, control)?
    } else {
        value
    };
    if let Some(value) = crate::assignment::domain::assign_domain_value_with_control(
        context, &value, None, target, control,
    )? {
        return Ok(value);
    }
    if matches!(*value, Value::Null) {
        return Ok(value);
    }
    let value = match &*value {
        Value::Datum(datum) => {
            crate::expr::datums::read_with_catalog_and_control(datum, Some(context), control)?
        }
        _ => value,
    };
    if let ColumnType::Array(element) = target {
        if super::type_requires_catalog_resolution(element) {
            return convert_catalog_array(context, value, element, control);
        }
    }
    if super::type_requires_catalog_resolution(target) {
        return crate::expr::cast_value_with_type_resolution_with_control(
            &value,
            None,
            &target.catalog_name_with_control(control)?,
            Some(context),
            control,
        );
    }
    super::convert_value_to_column_type_from_with_control(value, target, source, control)
}

fn convert_catalog_array(
    context: &dyn AssignmentContext,
    value: Produced<Value>,
    element: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let parsed = match &*value {
        Value::Array(_) => None,
        Value::Str(text) => Some(crate::expr::parse_pg_array_literal_with_control(
            text, control,
        )?),
        other => {
            let (message, _memory) = control
                .format(format_args!("expected an array, got {other:?}"))?
                .into_parts();
            return Err(SQLError::TypeMismatch(message));
        }
    };
    let array = match (&*value, parsed.as_deref()) {
        (_, Some(array)) | (Value::Array(array), None) => array,
        _ => unreachable!("array input was validated"),
    };
    let values = convert_array_elements(context, array.elements(), element, control)?;
    let mut bounds = ProductionVec::new(*control);
    bounds.extend_from_slice(array.lower_bounds())?;
    let array = ArrayValue::with_lower_bounds_with_control(values, bounds.finish()?, control)?
        .ok_or_else(|| {
            SQLError::TypeMismatch("multidimensional arrays must have matching dimensions".into())
        })?;
    let (array, memory) = array.into_parts();
    Ok(control.finish(
        Value::Array(array.with_element_type_oid(Some(
            crate::catalog::type_metadata::pg_type_oid(super::array_scalar_type(element)) as u32,
        ))),
        memory,
    )?)
}

fn convert_array_elements(
    context: &dyn AssignmentContext,
    values: &[Value],
    mut element: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>, SQLError> {
    while let ColumnType::Array(nested) = element {
        element = nested;
    }
    let mut output = ProductionVec::new(*control);
    output.reserve(values.len())?;
    for value in values {
        let value = match value {
            Value::List(values) => {
                let (values, memory) =
                    convert_array_elements(context, values, element, control)?.into_parts();
                control.finish(Value::List(values), memory)?
            }
            value => {
                convert_with_catalog(context, control.copy_value(value)?, element, None, control)?
            }
        };
        output.push_produced(value)?;
    }
    Ok(output.finish()?)
}
