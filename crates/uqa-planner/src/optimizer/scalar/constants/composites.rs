//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fold named ROW constructors only with the current catalog and admitted constant arguments.

use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::{ColumnType, SQLError, ScalarExpr};

pub(super) fn constant_field(
    value: &Value,
    name: &str,
    field: &uqa_sql::ast::CompositeFieldBinding,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
) -> Option<Result<Value, SQLError>> {
    if matches!(value, Value::Null) || field.dropped {
        return Some(Ok(Value::Null));
    }
    if let Err(error) = uqa_sql::expr::composites::validate_field_result(field) {
        return Some(Err(error));
    }
    if let Value::Datum(datum) = value {
        let selected = match uqa_sql::expr::datums::read_composite_field_with_control(
            datum,
            name,
            types?.composite_types(),
            &ProductionControl::uncontrolled(),
        ) {
            Ok(value) => value,
            Err(error) => return Some(Err(error)),
        };
        return Some(uqa_sql::expr::datums::copy_constant_field(
            &selected,
            &field.result_type,
        ));
    }
    match value {
        Value::Null => Some(Ok(Value::Null)),
        Value::Record(fields) => {
            if fields.type_oid().is_some_and(|oid| oid != field.type_oid) {
                return None;
            }
            Some(uqa_sql::expr::datums::copy_constant_field(
                fields.iter().find(|(key, _)| key.as_str() == name)?.1,
                &field.result_type,
            ))
        }
        _ => None,
    }
}

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
    if !items.iter().try_fold(true, |all, item| {
        Ok::<_, SQLError>(all && super::literal_value(item)?.is_some())
    })? {
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
                .copy_value(
                    super::literal_value(&items[index])?
                        .as_deref()
                        .expect("constant argument"),
                )
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
        collation_oid: None,
        composite_source: None,
    })
}
