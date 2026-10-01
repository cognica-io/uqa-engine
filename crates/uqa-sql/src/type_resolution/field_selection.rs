//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The type of `(expression).field`, as `ParseComplexProjection` resolves it: a whole-row reference selects the relation's column, an anonymous row selects its `fN` field, and a composite value selects its attribute. Otherwise `unknown_attribute` reports the missing field in the form that matches the expression.

use crate::ast::ColumnType;
use crate::SQLError;

use super::FunctionTypeResolver;

/// The field of a whole-row reference that does not exist, named by the reference's alias.
#[must_use]
pub fn missing_relation_column(qualifier: &str, field: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42703".into(),
        message: format!("column {qualifier}.{field} does not exist"),
    }
}

fn undefined_record_field(field: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42703".into(),
        message: format!("could not identify column \"{field}\" in record data type"),
    }
}

/// The zero-based position of an anonymous row's `fN` field.
#[must_use]
pub fn anonymous_field_position(field: &str, width: usize) -> Option<usize> {
    let position = field.strip_prefix('f')?;
    if position.starts_with('0') || !position.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    position
        .parse::<usize>()
        .ok()
        .and_then(|position| position.checked_sub(1))
        .filter(|position| *position < width)
}

/// The type of a field of a value of type `base`, for expressions that are neither whole-row references nor row constructors. `None` is an unresolved type: a dynamic document value, or a catalog that binding cannot consult.
pub fn value_field_type(
    base: Option<&ColumnType>,
    field: &str,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let Some(base) = base else {
        return Ok(None);
    };
    let mut composite = base;
    while let ColumnType::Domain { base, .. } = composite {
        composite = base;
    }
    match composite {
        ColumnType::Composite(reference) => {
            let Some(catalog) = resolver.and_then(FunctionTypeResolver::composite_types) else {
                return Ok(None);
            };
            let descriptor = crate::expr::composites::descriptor(Some(catalog), reference.oid)?;
            descriptor
                .attribute(field)
                .map(|(_, attribute)| Some(attribute.ty.clone()))
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!(
                        "column \"{field}\" not found in data type {}",
                        base.display_name()
                    ),
                })
        }
        ColumnType::Record => Err(undefined_record_field(field)),
        other => Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!(
                "column notation .{field} applied to type {}, which is not a composite type",
                other.display_name()
            ),
        }),
    }
}

/// The field of a folded row constant: an anonymous row's `fN` field or a named record's field.
pub fn literal_field_type(
    value: &uqa_core::Value,
    field: &str,
) -> Option<Result<Option<ColumnType>, SQLError>> {
    match value {
        uqa_core::Value::Row(values) => Some(row_field_type(
            &values
                .iter()
                .map(super::common::value_type)
                .collect::<Vec<_>>(),
            field,
        )),
        uqa_core::Value::Record(fields) => Some(
            fields
                .iter()
                .find(|(name, _)| name == field)
                .map(|(_, value)| super::common::value_type(value))
                .ok_or_else(|| undefined_record_field(field)),
        ),
        _ => None,
    }
}

/// The type of an anonymous row constructor's field, given the types of its items.
pub fn row_field_type(
    items: &[Option<ColumnType>],
    field: &str,
) -> Result<Option<ColumnType>, SQLError> {
    anonymous_field_position(field, items.len())
        .map(|position| items[position].clone())
        .ok_or_else(|| undefined_record_field(field))
}
