//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constant replacement retains admitted scalar and type owners until the completed result replaces its input.

use super::{
    arithmetic_needs_result_type, composites, eligibility, literal_value_with_control,
    strict_null_expression, BinaryOp, ColumnType, Produced, ProductionControl, RowSchema, SQLError,
    ScalarExpr, Value,
};
use crate::optimizer::scalar::ScalarProduction;
use crate::OptimizerConfig;
use uqa_sql::{
    ir::{RetainedConstantValue, RetainedScalarLiteral},
    plan::OwnedScalarExpr,
};

mod storage;
pub(in crate::optimizer::scalar) use storage::retain_large_literal;

pub(in crate::optimizer::scalar) fn fold(
    expression: OwnedScalarExpr,
    result_type: Option<&ColumnType>,
    production: ScalarProduction<'_, '_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let config = production.config;
    let permissions = config.builtin_permissions.as_deref();
    let types = config
        .routine_inlining
        .as_ref()
        .map(|context| context.types);
    let resolver = config
        .routine_inlining
        .as_ref()
        .map(|context| context.routines as &dyn uqa_sql::FunctionTypeResolver);
    fold_selected(
        expression,
        result_type,
        production,
        permissions,
        types,
        resolver,
        |expression, control| (config.constant_evaluator)(expression, &[], control),
    )
}

pub(super) fn fold_selected(
    expression: OwnedScalarExpr,
    result_type: Option<&ColumnType>,
    production: ScalarProduction<'_, '_>,
    permissions: Option<&dyn uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution>,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
    resolver: Option<&dyn uqa_sql::FunctionTypeResolver>,
    evaluate: impl FnOnce(&ScalarExpr, &ProductionControl<'_>) -> Result<Produced<Value>, SQLError>,
) -> Result<OwnedScalarExpr, SQLError> {
    let config = production.config;
    let control = &config.production;
    control.check()?;
    if matches!(&*expression, ScalarExpr::Func { binding: Some(binding), .. }
        if binding.dispatch.is_some_and(uqa_sql::ast::FunctionDispatch::is_call_argument_marker))
    {
        return Ok(expression);
    }
    if let Some(domain) = super::domains::fold(&expression, config)? {
        return production.adopt(retained_result(
            domain.value,
            Some(domain.ty),
            true,
            config,
        )?);
    }
    if let ScalarExpr::Cast { expr, ty, .. } = &*expression {
        if literal_value_with_control(expr, control)?
            .as_deref()
            .is_some_and(|value| matches!(value, Value::Null))
        {
            if let Some(target) = eligibility::direct_null_cast_type(ty, types, control)? {
                return production.adopt(OwnedScalarExpr::typed_literal(
                    control.finish(Value::Null, control.empty_reservation())?,
                    control.copy_text(ty)?,
                    Some(target),
                    None,
                    None,
                    control,
                )?);
            }
        }
    }
    if let Some(literal) = composites::owned::fold_field(&expression, types, control) {
        return production.adopt(literal?);
    }
    let strict_null = strict_null_expression(&expression, control)?;
    if matches!(&*expression, ScalarExpr::RetainedLiteral(_))
        || literal_value_with_control(&expression, control)?.is_some()
        || (!strict_null && !eligibility::is_constant(&expression, types, resolver, control)?)
        || (strict_null && arithmetic_needs_result_type(&expression, result_type))
    {
        return Ok(expression);
    }
    let selected = strict_null_result_type(&expression, strict_null, control)?;
    let ty = match result_type {
        Some(ty) => Some(ty.clone_with_control(control)?),
        None => selected,
    };
    let ty = match ty {
        Some(ty) => Some(ty),
        None => eligibility::scalar_type(&expression, resolver, control)?,
    };
    let (value, was_cast) = if strict_null {
        // The selected type and the NULL argument determine the result.
        drop(expression);
        (
            control.finish(Value::Null, control.empty_reservation())?,
            false,
        )
    } else {
        let empty = RowSchema::new_with_control(
            control.finish(Vec::new(), control.empty_reservation())?,
            control,
        )?;
        let expression = uqa_sql::bind_type_introspection_owned(
            expression,
            &empty,
            &[],
            resolver,
            &production.storage,
        )?;
        if let (
            Some(permissions),
            ScalarExpr::Func {
                binding: Some(binding),
                ..
            },
        ) = (permissions, &*expression)
        {
            permissions.require_execute_with_control(binding, control)?;
        }
        (
            evaluate(&expression, control)?,
            matches!(&*expression, ScalarExpr::Cast { .. }),
        )
    };
    let literal = OwnedScalarExpr::literal(value, control)?;
    let untyped = !matches!(
        &*literal,
        ScalarExpr::Literal(Value::Int(_) | Value::Str(_))
    ) && !was_cast
        && eligibility::scalar_type(&literal, resolver, control)?.as_deref() == ty.as_deref();
    let value = literal.into_literal_value(control)?;
    production.adopt(retained_result(value, ty, !untyped, config)?)
}

fn retained_result(
    value: Produced<Value>,
    ty: Option<Produced<ColumnType>>,
    declared: bool,
    config: &OptimizerConfig<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let control = &config.production;
    let declaration = if declared {
        ty.as_ref()
            .map(|ty| ty.catalog_name_with_control(control))
            .transpose()?
    } else {
        None
    };
    if control.budget().is_some() && value.reserved_bytes() > 0 {
        let storage = storage::select(&value, ty.as_deref(), config)?;
        let value = match storage {
            Some(storage) => storage.retain_produced(value, control)?,
            None => RetainedConstantValue::from_produced(value, control)?,
        };
        return Ok(OwnedScalarExpr::retained_literal(
            RetainedScalarLiteral::with_control(value, ty, declaration, None, control)?,
            control,
        )?);
    }
    match declaration {
        Some(declaration) => Ok(OwnedScalarExpr::typed_literal(
            value,
            declaration,
            ty,
            None,
            None,
            control,
        )?),
        None => Ok(OwnedScalarExpr::literal(value, control)?),
    }
}

fn strict_null_result_type(
    expression: &ScalarExpr,
    strict_null: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match expression {
        ScalarExpr::Binary {
            op:
                BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual,
            ..
        } if strict_null => Ok(Some(
            control.finish(ColumnType::Boolean, control.empty_reservation())?,
        )),
        ScalarExpr::Func {
            binding: Some(binding),
            ..
        } if strict_null => {
            if let Some(error) = &binding.resolution_error {
                return Err(error.sql_error());
            }
            uqa_sql::fixed_builtin_return_type_with_control(binding, control)
        }
        _ => Ok(None),
    }
}
