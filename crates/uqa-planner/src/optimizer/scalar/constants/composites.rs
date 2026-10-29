//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fold named ROW constructors only with the current catalog and admitted constant arguments.

use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::ir::{RetainedConstantValue, RetainedScalarLiteral};
use uqa_sql::routines::declaration::RoutineTypeCatalog;
use uqa_sql::{ColumnType, SQLError, ScalarExpr};

pub(super) mod owned;

struct Field<'a> {
    type_oid: u32,
    ty: &'a ColumnType,
    collation: Option<u32>,
    binding: Option<&'a uqa_sql::ast::CompositeFieldBinding>,
}

fn constant_field(
    value: &Value,
    name: &str,
    field: &Field<'_>,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>, SQLError>> {
    if matches!(value, Value::Null) || field.binding.is_some_and(|field| field.dropped) {
        return Some(
            control
                .finish(Value::Null, control.empty_reservation())
                .map_err(Into::into),
        );
    }
    if let Some(binding) = field.binding {
        if let Err(error) = uqa_sql::expr::composites::validate_field_result(binding) {
            return Some(Err(error));
        }
    }
    if let Value::Datum(datum) = value {
        let selected = match uqa_sql::expr::datums::read_composite_field_with_control(
            datum,
            name,
            types?.composite_types(),
            control,
        ) {
            Ok(value) => value,
            Err(error) => return Some(Err(error)),
        };
        return Some(uqa_sql::expr::datums::copy_constant_field_with_control(
            &selected, field.ty, control,
        ));
    }
    match value {
        Value::Record(fields) => {
            if fields.type_oid().is_some_and(|oid| oid != field.type_oid) {
                return None;
            }
            Some(uqa_sql::expr::datums::copy_constant_field_with_control(
                fields.iter().find(|(key, _)| key.as_str() == name)?.1,
                field.ty,
                control,
            ))
        }
        _ => None,
    }
}

pub(in crate::optimizer::scalar) fn fold_composite_constructor(
    expression: ScalarExpr,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<ScalarExpr, SQLError> {
    match owned::fold_constructor(&expression, types, control)? {
        Some(replacement) => Ok(replacement.finish(control)?.into_parts().0),
        None => Ok(expression),
    }
}

struct Composite<'a> {
    value: &'a Value,
    type_oid: u32,
    declaration: Option<&'a str>,
    original: bool,
}

fn composite(expression: &ScalarExpr) -> Option<Composite<'_>> {
    let (value, ty, declaration, original) = match expression {
        ScalarExpr::TypedLiteral {
            value,
            bound_type: Some(ty),
            ty: declaration,
            composite_source,
            ..
        } => (
            value,
            ty,
            Some(declaration.as_str()),
            composite_source.is_some(),
        ),
        ScalarExpr::RetainedLiteral(literal) => (
            literal.value.resident_value()?,
            literal.ty.as_ref()?,
            literal.declaration.as_deref(),
            false,
        ),
        _ => return None,
    };
    let ColumnType::Composite(reference) = ty else {
        return None;
    };
    Some(Composite {
        value,
        type_oid: reference.oid,
        declaration,
        original,
    })
}

pub(super) fn fold_field(
    expression: &ScalarExpr,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Option<Result<ScalarExpr, SQLError>> {
    owned::fold_field(expression, types, control)
        .map(|replacement| Ok(replacement?.finish(control)?.into_parts().0))
}
