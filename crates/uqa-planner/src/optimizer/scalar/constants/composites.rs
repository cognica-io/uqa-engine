//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fold named ROW constructors only with the current catalog and admitted constant arguments.

use uqa_core::memory::ProductionControl;
use uqa_sql::{ColumnType, SQLError, ScalarExpr};

pub(in crate::optimizer::scalar) fn fold_composite_constructor(
    expression: ScalarExpr,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
) -> Result<ScalarExpr, SQLError> {
    let Some(types) = types else {
        return Ok(expression);
    };
    let ScalarExpr::CompositeRow {
        items,
        binding,
        bound_type,
    } = &expression
    else {
        return Ok(expression);
    };
    if !items
        .iter()
        .all(|item| super::literal_value(item).is_some())
    {
        return Ok(expression);
    }
    let ty = match bound_type {
        Some(ty) => ty.clone(),
        None => types.resolve_catalog_column_type_name(&binding.ty)?,
    };
    let ColumnType::Composite(reference) = &ty else {
        return Ok(expression);
    };
    let descriptor = uqa_sql::expr::composites::descriptor(types.composite_types(), reference.oid)?;
    let control = ProductionControl::uncontrolled();
    let value = uqa_sql::expr::composites::constructor::construct_with_control(
        binding,
        items.len(),
        &descriptor,
        &control,
        |index| {
            control
                .copy_value(super::literal_value(&items[index]).expect("constant argument"))
                .map_err(Into::into)
        },
    )?
    .into_uncontrolled()
    .expect("ordinary constant evaluation");
    Ok(ScalarExpr::TypedLiteral {
        value,
        ty: binding.ty.clone(),
        bound_type: Some(ty),
        parameter_index: None,
        composite_source: None,
    })
}
