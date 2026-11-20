//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate stored CHECK syntax in its original row or domain VALUE scope.

use crate::{
    ast::{ColumnDef, CreateDomain, TableCheck},
    catalog::scalar_bindings::{validate_boolean_with_work, NativeScalarContext},
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
    crate::catalog::work::inline(validate_table_with_work(
        name,
        columns,
        checks,
        row_type,
        context,
        control,
        &mut (),
    ))
}

pub async fn validate_table_with_work(
    name: &RelationIdentity,
    columns: &[ColumnDef],
    checks: &[TableCheck],
    row_type: Option<&ColumnType>,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
) -> Result<(), SQLError> {
    control.check()?;
    let mut first_column_check = None;
    for (position, column) in columns.iter().enumerate() {
        work.checkpoint().await?;
        control.check()?;
        if column.check.is_some() {
            first_column_check = Some(position);
            break;
        }
    }
    if first_column_check.is_none() && checks.is_empty() {
        return Ok(());
    }
    let schema =
        super::super::check_row_schema_with_control(&name.name, columns, row_type, control)?;
    for column in &columns[first_column_check.unwrap_or(columns.len())..] {
        work.checkpoint().await?;
        control.check()?;
        if let Some(expression) = &column.check {
            validate_boolean_with_work(expression, &schema, context, control, work).await?;
        }
    }
    for check in checks {
        work.checkpoint().await?;
        validate_boolean_with_work(&check.expr, &schema, context, control, work).await?;
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
    crate::catalog::work::inline(validate_domain_with_work(domain, context, control, &mut ()))
}

pub async fn validate_domain_with_work(
    domain: &CreateDomain,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
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
        work.checkpoint().await?;
        validate_boolean_with_work(&check.expression, &schema, context, control, work).await?;
    }
    control.check()?;
    Ok(())
}

#[cfg(test)]
mod tests;
