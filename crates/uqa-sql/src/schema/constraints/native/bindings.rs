//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate stored CHECK syntax in its original row or domain VALUE scope.

use crate::{
    ast::{ColumnDef, CreateDomain, TableCheck},
    catalog::scalar_bindings::{validate_boolean, NativeScalarContext},
    ColumnType, RelationIdentity, RowSchema, SQLError,
};
use uqa_core::memory::ProductionControl;

/// Ordinary and foreign tables share CHECK semantics, including named whole rows and tableoid. Validation never reads rows or changes NOT VALID, enforcement or inheritance flags.
pub fn validate_table(
    name: &RelationIdentity,
    columns: &[ColumnDef],
    checks: &[TableCheck],
    row_type: Option<&ColumnType>,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let mut expressions = columns
        .iter()
        .filter_map(|column| column.check.as_ref())
        .chain(checks.iter().map(|check| &check.expr))
        .peekable();
    if expressions.peek().is_none() {
        return Ok(());
    }
    let schema =
        super::super::check_row_schema_with_control(&name.name, columns, row_type, control)?;
    for expression in expressions {
        validate_boolean(expression, &schema, context, control)?;
    }
    control.check()?;
    Ok(())
}

/// Domain CHECKs expose only VALUE with the original base type; they do not invoke domain checks or validate existing values.
pub fn validate_domain(
    domain: &CreateDomain,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if domain.checks.is_empty() {
        return Ok(());
    }
    let schema = RowSchema::with_column_metadata_with_control(
        None,
        [("value", Some(&domain.base), domain.collation_oid)],
        control,
    )?;
    for check in &domain.checks {
        validate_boolean(&check.expression, &schema, context, control)?;
    }
    control.check()?;
    Ok(())
}

#[cfg(test)]
mod tests;
