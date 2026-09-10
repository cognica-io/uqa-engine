//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain base conversion and inherited constraint evaluation.

use super::AssignmentContext;
use crate::{ColumnType, ResultRow, RowSchema, SQLError};
use uqa_core::Value;

pub fn cast_domain_value(
    context: &dyn AssignmentContext,
    value: &Value,
    source: Option<&str>,
    ty: &ColumnType,
) -> Result<Option<Value>, SQLError> {
    convert_domain_value(context, value, source, ty, false)
}

pub fn assign_domain_value(
    context: &dyn AssignmentContext,
    value: &Value,
    ty: &ColumnType,
) -> Result<Option<Value>, SQLError> {
    convert_domain_value(context, value, None, ty, true)
}

fn convert_domain_value(
    context: &dyn AssignmentContext,
    value: &Value,
    source: Option<&str>,
    ty: &ColumnType,
    assignment: bool,
) -> Result<Option<Value>, SQLError> {
    let ColumnType::Domain { oid, .. } = ty else {
        return Ok(None);
    };
    let Some(domain) = context.domain_by_oid(*oid) else {
        return Ok(None);
    };
    if source
        .and_then(|name| context.resolve_type_name(name).ok().flatten())
        .as_ref()
        == Some(ty)
    {
        return Ok(Some(value.clone()));
    }
    let mut chain = vec![domain.clone()];
    let mut base = domain.definition.base.clone();
    while let ColumnType::Domain {
        oid,
        base: underlying,
        ..
    } = &base
    {
        if let Some(parent) = context.domain_by_oid(*oid) {
            base = parent.definition.base.clone();
            chain.push(parent);
        } else {
            base = *underlying.clone();
        }
    }
    let value = if assignment {
        super::conversion::convert_value_to_column_type_with_context(context, value.clone(), &base)?
    } else {
        crate::expr::cast_value_with_type_resolution(
            value,
            source,
            &base.sql_name(),
            Some(context),
        )?
    };
    if matches!(value, Value::Null)
        && chain
            .iter()
            .any(|domain| domain.definition.not_null.is_some())
    {
        return Err(domain_error(
            "23502",
            format!(
                "domain {} does not allow null values",
                domain_display_name(context, domain.oid)?
            ),
        ));
    }
    let row = ResultRow::from([("value".into(), value.clone())]);
    let schema = RowSchema::with_types(
        vec!["value".into()],
        vec![Some(domain.definition.base.clone())],
    );
    for check in chain
        .iter()
        .rev()
        .flat_map(|domain| &domain.definition.checks)
    {
        let result = context.evaluate_domain_check(&check.expression, &row, &schema)?;
        if result == Value::Bool(false) {
            return Err(domain_error(
                "23514",
                format!(
                    "value for domain {} violates check constraint \"{}\"",
                    domain_display_name(context, domain.oid)?,
                    check.name.as_deref().expect("bound domain constraint")
                ),
            ));
        }
    }
    Ok(Some(value))
}

fn domain_display_name(context: &dyn AssignmentContext, oid: u32) -> Result<String, SQLError> {
    context
        .resolve_regtype_output(&ColumnType::Regtype, i64::from(oid))
        .map_err(SQLError::Internal)?
        .ok_or_else(|| SQLError::Internal("domain type has no catalog display name".into()))
}

pub fn domain_error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
