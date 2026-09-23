//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog OID output preserves type-specific spelling and admitted array construction.

use super::super::conversion::{array_value_to_string_with_control, value_to_string_with_control};
use super::{casting::rebuild_array, EngineHook};
use crate::{
    ast::ColumnType,
    error::{Result, SQLError},
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

/// Format a scalar or array OID carrier using the catalog-aware output function of a `reg*` type. `None` means the declared type is not one of the supported alias types or the value is SQL NULL.
pub fn format_regtype_value(
    value: &Value,
    ty: &ColumnType,
    engine: Option<&dyn EngineHook>,
) -> Result<Option<String>> {
    format_regtype_value_with_control(value, ty, engine, &ProductionControl::uncontrolled())?
        .map(|text| {
            text.into_uncontrolled()
                .map_err(|_| SQLError::Internal("ordinary OID output owner".into()))
        })
        .transpose()
}

/// Retain a resolver's returned spelling and construct SQL-owned array and fallback text under the invoking allowance.
pub fn format_regtype_value_with_control(
    value: &Value,
    ty: &ColumnType,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<String>>> {
    control.check()?;
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    if let ColumnType::Array(element) = ty {
        if !is_regtype(element) {
            return Ok(None);
        }
        let Value::Array(array) = value else {
            return value_to_string_with_control(value, control).map(Some);
        };
        let elements = format_array_elements(array.elements(), element, engine, control)?;
        let formatted = rebuild_array(array, elements, control)?.ok_or_else(|| {
            SQLError::Internal("regtype array output changed the array dimensions".into())
        })?;
        return array_value_to_string_with_control(&formatted, control).map(Some);
    }
    if !is_regtype(ty) {
        return Ok(None);
    }
    let Value::Int(oid) = value else {
        return value_to_string_with_control(value, control).map(Some);
    };
    if *oid == 0 {
        return Ok(Some(control.copy_text("-")?));
    }
    let resolved = engine
        .map(|engine| engine.resolve_regtype_output(ty, *oid))
        .transpose()
        .map_err(SQLError::Internal)?
        .flatten();
    match resolved {
        Some(text) => {
            let (value, memory) = control
                .retain_external_value(Value::Str(text))?
                .into_parts();
            let Value::Str(text) = value else {
                unreachable!("external OID spelling remains text")
            };
            Ok(Some(control.finish(text, memory)?))
        }
        None => Ok(Some(control.format(format_args!("{oid}"))?)),
    }
}

fn is_regtype(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regclass
            | ColumnType::Regnamespace
            | ColumnType::Regrole
            | ColumnType::Regtype
    )
}

fn format_array_elements(
    values: &[Value],
    element: &ColumnType,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    let mut output = ProductionVec::new(*control);
    output.reserve(values.len())?;
    for value in values {
        let value = match value {
            Value::Null => control.finish(Value::Null, control.empty_reservation())?,
            Value::List(nested) => {
                let (nested, memory) =
                    format_array_elements(nested, element, engine, control)?.into_parts();
                control.finish(Value::List(nested), memory)?
            }
            value => match format_regtype_value_with_control(value, element, engine, control)? {
                Some(text) => {
                    let (text, memory) = text.into_parts();
                    control.finish(Value::Str(text), memory)?
                }
                None => control.copy_value(value)?,
            },
        };
        output.push_produced(value)?;
    }
    Ok(output.finish()?)
}
