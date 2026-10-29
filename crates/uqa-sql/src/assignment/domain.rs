//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain base conversion and inherited constraint evaluation.

use super::AssignmentContext;
use crate::{ColumnType, RowSchema, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

mod diagnostics;

#[cfg(test)]
mod tests;

pub fn cast_domain_value(
    context: &dyn AssignmentContext,
    value: &Value,
    source: Option<&str>,
    ty: &ColumnType,
) -> Result<Option<Value>, SQLError> {
    convert_domain_value(context, value, source, ty, false)
}

/// Domain input retains the caller's resource owners through base conversion, tuple admission and every constraint callback.
pub fn cast_domain_value_with_control(
    context: &dyn AssignmentContext,
    value: &Value,
    source: Option<&str>,
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    convert_domain_value_with_control(context, value, source, ty, false, control)
}

pub fn assign_domain_value_with_control(
    context: &dyn AssignmentContext,
    value: &Value,
    source: Option<&str>,
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    convert_domain_value_with_control(context, value, source, ty, true, control)
}

pub fn assign_domain_value(
    context: &dyn AssignmentContext,
    value: &Value,
    ty: &ColumnType,
) -> Result<Option<Value>, SQLError> {
    assign_domain_value_from(context, value, None, ty)
}

pub(super) fn assign_domain_value_from(
    context: &dyn AssignmentContext,
    value: &Value,
    source: Option<&str>,
    ty: &ColumnType,
) -> Result<Option<Value>, SQLError> {
    convert_domain_value(context, value, source, ty, true)
}

fn convert_domain_value(
    context: &dyn AssignmentContext,
    value: &Value,
    source: Option<&str>,
    ty: &ColumnType,
    assignment: bool,
) -> Result<Option<Value>, SQLError> {
    convert_domain_value_with_control(
        context,
        value,
        source,
        ty,
        assignment,
        &ProductionControl::uncontrolled(),
    )?
    .map(|value| {
        value
            .into_uncontrolled()
            .map_err(|_| SQLError::Internal("ordinary domain value retention".into()))
    })
    .transpose()
}

fn convert_domain_value_with_control(
    context: &dyn AssignmentContext,
    value: &Value,
    source: Option<&str>,
    ty: &ColumnType,
    assignment: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    control.check()?;
    let ColumnType::Domain { oid, .. } = ty else {
        return Ok(None);
    };
    let Some(domain) = context.domain_by_oid_with_control(*oid, control)? else {
        return Ok(None);
    };
    let source_type = source
        .map(|name| context.resolve_type_name_with_control(name, control))
        .transpose();
    let source_type = match source_type {
        Ok(ty) => ty.flatten(),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        Err(_) => None,
    };
    if source_type.as_deref() == Some(ty) {
        return Ok(Some(
            super::conversion::normalize_legacy_vector_carrier_with_control(value, ty, control)?
                .map_or_else(|| control.copy_value(value), Ok)?,
        ));
    }
    let mut base = domain.definition.base.clone_with_control(control)?;
    let mut chain = ProductionVec::new(*control);
    chain.push_produced(domain)?;
    while let ColumnType::Domain {
        oid,
        base: underlying,
        ..
    } = &*base
    {
        control.check()?;
        if let Some(parent) = context.domain_by_oid_with_control(*oid, control)? {
            base = parent.definition.base.clone_with_control(control)?;
            chain.push_produced(parent)?;
        } else {
            base = underlying.clone_with_control(control)?;
        }
    }
    let value = if assignment {
        let source = source
            .map(|source| context.resolve_type_name_with_control(source, control))
            .transpose()?
            .flatten();
        super::conversion::coerce_assignment_value_with_control(
            context,
            control.copy_value(value)?,
            &base,
            source.as_deref(),
            control,
        )?
    } else {
        crate::expr::cast_value_with_type_resolution_with_control(
            value,
            source,
            &base.catalog_name_with_control(control)?,
            Some(context),
            control,
        )?
    };
    check_value(context, value, &chain, control).map(Some)
}

fn check_value(
    context: &dyn AssignmentContext,
    value: Produced<Value>,
    chain: &[crate::catalog::domain::StoredDomain],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    if matches!(*value, Value::Null)
        && chain
            .iter()
            .any(|domain| domain.definition.not_null.is_some())
    {
        return Err(diagnostics::violation(
            context,
            chain[0].oid,
            None,
            control,
        )?);
    }
    // Checks may change the catalog. Retain the admitted tuple before invoking them.
    let value = crate::expr::composites::retain_value_with_control(
        value,
        context.composite_types(),
        control,
    )?;
    let row = DomainValue(&value);
    let schema = RowSchema::with_column_metadata_with_control(
        None,
        [("value", Some(&chain[0].definition.base), None)],
        control,
    )?;
    for check in chain
        .iter()
        .rev()
        .flat_map(|domain| &domain.definition.checks)
    {
        control.check()?;
        let result = context.evaluate_domain_check_with_control(
            &check.expression,
            &row,
            &schema,
            control,
        )?;
        control.check()?;
        if *result == Value::Bool(false) {
            return Err(diagnostics::violation(
                context,
                chain[0].oid,
                Some(check.name.as_deref().expect("bound domain constraint")),
                control,
            )?);
        }
    }
    Ok(value)
}

struct DomainValue<'a>(&'a Value);

impl crate::expr::RowLookup for DomainValue<'_> {
    fn column(&self, name: &str) -> Option<&Value> {
        (name == "value").then_some(self.0)
    }

    fn qualified_column(&self, _: &str, _: &str) -> Option<&Value> {
        None
    }

    fn positional_column(&self, index: usize) -> Option<&Value> {
        (index == 0).then_some(self.0)
    }
}

pub fn domain_error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
