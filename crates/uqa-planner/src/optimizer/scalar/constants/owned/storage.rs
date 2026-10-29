//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constant storage selection borrows complete owners and publishes retained values before retiring decoded inputs.

use super::{
    ColumnType, OptimizerConfig, OwnedScalarExpr, Produced, ProductionControl,
    RetainedScalarLiteral, SQLError, ScalarExpr, Value,
};
use uqa_core::memory::MemoryBudget;
use uqa_sql::ir::ConstantValueStorage;

fn builtin_type(ty: &ColumnType, control: &ProductionControl<'_>) -> Result<bool, SQLError> {
    let name = ty.catalog_name_with_control(control)?;
    Ok(
        uqa_sql::type_resolution::pg_catalog_builtin_type_with_control(&name, control)?.as_deref()
            == Some(ty),
    )
}

pub(super) fn select<'a>(
    value: &Produced<Value>,
    ty: Option<&ColumnType>,
    config: &'a OptimizerConfig<'_>,
) -> Result<Option<&'a dyn ConstantValueStorage>, SQLError> {
    if !config.retain_values_constants
        || value.reserved_bytes() <= size_of::<ScalarExpr>()
        || uqa_sql::expr::composites::literal::contains_records(value)
        || uqa_sql::expr::datums::contains_datum(value)
    {
        return Ok(None);
    }
    let Some(storage) = config.constant_storage.as_deref() else {
        return Ok(None);
    };
    match ty {
        Some(ty) if builtin_type(ty, &config.production)? => Ok(Some(storage)),
        _ => Ok(None),
    }
}

pub(in crate::optimizer::scalar) fn retain_large_literal(
    expression: OwnedScalarExpr,
    config: &OptimizerConfig<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let Some(storage) = config.constant_storage.as_deref() else {
        return Ok(expression);
    };
    let (value, selected) = match &*expression {
        ScalarExpr::TypedLiteral {
            value,
            bound_type: Some(ty),
            parameter_index: None,
            composite_source: None,
            ..
        } => (value, Some(ty)),
        ScalarExpr::Literal(value) => (value, None),
        _ => return Ok(expression),
    };
    let control = &config.production;
    let ordinary;
    let scratch = match control.budget() {
        Some(budget) => budget,
        None => {
            ordinary = MemoryBudget::new(usize::MAX);
            &ordinary
        }
    };
    let bytes =
        value.retained_payload_bytes_with_check(scratch, || control.check_cancellation())?;
    if bytes <= size_of::<ScalarExpr>()
        || uqa_sql::expr::composites::literal::contains_records(value)
        || uqa_sql::expr::datums::contains_datum(value)
    {
        return Ok(expression);
    }
    let inferred;
    let ty = if let Some(ty) = selected {
        ty
    } else {
        inferred = super::eligibility::scalar_type(&expression, None, control)?;
        let Some(ty) = inferred.as_deref() else {
            return Ok(expression);
        };
        ty
    };
    if !builtin_type(ty, control)? {
        return Ok(expression);
    }
    let ty = ty.clone_with_control(control)?;
    let (declaration, collation_oid) = match &*expression {
        ScalarExpr::TypedLiteral {
            ty, collation_oid, ..
        } => (Some(control.copy_text(ty)?), *collation_oid),
        ScalarExpr::Literal(_) => (None, None),
        _ => unreachable!("selected literal"),
    };
    // An existing typed leaf owns its allocating fields together. Copy the surviving value through its producer, retaining the original leaf until the store publishes the new owner.
    let value = control.copy_value(value)?;
    let retained = storage.retain_produced(value, control)?;
    let output = OwnedScalarExpr::retained_literal(
        RetainedScalarLiteral::with_control(
            retained,
            Some(ty),
            declaration,
            collation_oid,
            control,
        )?,
        control,
    )?;
    drop(expression);
    Ok(output)
}
