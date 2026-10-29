//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OID input keeps parsed text and the admitted result independent, preserving parse-before-lookup diagnostics.

use super::{is_alias, OidAliasInput};
use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue, Value,
};

/// Bind explicit alias casts and sequence-name arguments in a private executable while retaining every replacement's constructor owner.
pub fn read_prepared_oid_alias_constants_owned(
    catalog: &dyn OidAliasInput,
    plan: crate::plan::MutableExecutablePlan,
    control: &ProductionControl<'_>,
) -> Result<crate::plan::MutableExecutablePlan, SQLError> {
    plan.transform_scalar_nodes(control, &mut |_, expression| {
        let selected = match &*expression {
            crate::ScalarExpr::Func { name, args, .. } if super::is_sequence_function(name) => {
                if let Some(crate::ScalarExpr::Literal(Value::Str(text))) = args.first() {
                    Some(typed_input(
                        catalog,
                        control.finish(ColumnType::Regclass, control.empty_reservation())?,
                        text,
                        control,
                    )?)
                } else {
                    None
                }
            }
            crate::ScalarExpr::Cast { expr, ty, .. } => {
                let crate::ScalarExpr::Literal(Value::Str(text)) = expr.as_ref() else {
                    return Ok(expression);
                };
                let Some(ty) = alias_type(ty, control)? else {
                    return Ok(expression);
                };
                Some(typed_input(catalog, ty, text, control)?)
            }
            _ => None,
        };
        if let Some(replacement) = selected {
            expression.map_child(0, control, |_| Ok(replacement))
        } else {
            Ok(expression)
        }
    })
}

fn alias_type(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    if !name
        .as_bytes()
        .windows(3)
        .any(|part| part.eq_ignore_ascii_case(b"reg"))
    {
        return Ok(None);
    }
    let ty = match ColumnType::from_sql_name_with_control(name, control) {
        Ok(ty) => ty,
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        Err(_) => return Ok(None),
    };
    Ok(match &*ty {
        ColumnType::Array(element) if is_alias(element) => Some(ty),
        scalar if is_alias(scalar) => Some(ty),
        _ => None,
    })
}

fn typed_input(
    catalog: &dyn OidAliasInput,
    ty: Produced<ColumnType>,
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<crate::plan::OwnedScalarExpr, SQLError> {
    let value = read_unknown_constant_with_control(catalog, &ty, text, control)?
        .expect("selected OID alias type");
    let name = ty.catalog_name_with_control(control)?;
    Ok(crate::plan::OwnedScalarExpr::typed_literal(
        value,
        name,
        Some(ty),
        None,
        None,
        control,
    )?)
}

pub(crate) fn read_unknown_constant_with_control(
    catalog: &dyn OidAliasInput,
    ty: &ColumnType,
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    control.check()?;
    match ty {
        ColumnType::Array(element)
            if is_alias(element) || matches!(element.as_ref(), ColumnType::Regrole) =>
        {
            read_constant_with_control(catalog, element, text, true, control).map(Some)
        }
        scalar if is_alias(scalar) || matches!(scalar, ColumnType::Regrole) => {
            read_constant_with_control(catalog, scalar, text, false, control).map(Some)
        }
        _ => Ok(None),
    }
}

pub(super) fn read_constant_with_control<C: OidAliasInput + ?Sized>(
    catalog: &C,
    ty: &ColumnType,
    text: &str,
    array: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    if !array {
        return Ok(control.finish(
            Value::Int(read_name(catalog, ty, text, control)?),
            control.empty_reservation(),
        )?);
    }
    let parsed = crate::expr::parse_pg_array_literal_with_control(text, control)?;
    let mut bounds = ProductionVec::new(*control);
    for bound in parsed.lower_bounds() {
        bounds.push_copy(*bound)?;
    }
    let elements = read_array_elements(catalog, ty, parsed.elements(), control)?;
    let output = ArrayValue::with_lower_bounds_with_control(elements, bounds.finish()?, control)?
        .ok_or_else(|| {
        formatted_error(
            format_args!("{} array literal lost its shape", ty.sql_name()),
            control,
            SQLError::Internal,
        )
    })?;
    let (array, memory) = output.into_parts();
    Ok(control.finish(Value::Array(array), memory)?)
}

fn read_array_elements<C: OidAliasInput + ?Sized>(
    catalog: &C,
    ty: &ColumnType,
    elements: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for element in elements {
        control.check()?;
        let value = match element {
            Value::Null => control.finish(Value::Null, control.empty_reservation())?,
            Value::Str(name) => control.finish(
                Value::Int(read_name(catalog, ty, name, control)?),
                control.empty_reservation(),
            )?,
            Value::List(elements) => {
                let (elements, memory) =
                    read_array_elements(catalog, ty, elements, control)?.into_parts();
                control.finish(Value::List(elements), memory)?
            }
            other => {
                return Err(formatted_error(
                    format_args!("cannot read {other:?} as {}", ty.sql_name()),
                    control,
                    SQLError::TypeMismatch,
                ))
            }
        };
        output.push_produced(value)?;
    }
    Ok(output.finish()?)
}

fn read_name<C: OidAliasInput + ?Sized>(
    catalog: &C,
    ty: &ColumnType,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<i64, SQLError> {
    catalog
        .resolve_oid_alias_input_with_control(ty, name, control)?
        .ok_or_else(|| {
            let (sqlstate, object) = match ty {
                ColumnType::Regclass => ("42P01", "relation"),
                ColumnType::Regtype => ("42704", "type"),
                ColumnType::Regrole => ("42704", "role"),
                ColumnType::Regproc | ColumnType::Regprocedure => ("42883", "function"),
                ColumnType::Regcollation => ("42704", "collation"),
                _ => ("3F000", "schema"),
            };
            let suffix = if matches!(ty, ColumnType::Regcollation) {
                " for encoding \"UTF8\""
            } else {
                ""
            };
            formatted_error(
                format_args!("{object} \"{name}\"{suffix} does not exist"),
                control,
                |message| SQLError::Routine {
                    sqlstate: sqlstate.into(),
                    message,
                },
            )
        })
}

fn formatted_error(
    message: std::fmt::Arguments<'_>,
    control: &ProductionControl<'_>,
    error: impl FnOnce(String) -> SQLError,
) -> SQLError {
    match control.format(message) {
        Ok(message) => error(message.into_parts().0),
        Err(error) => error.into(),
    }
}
