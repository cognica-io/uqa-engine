//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluation of immutable constants without erasing their declared SQL type.

use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::SQLError;
use uqa_sql::{scalar_type, RowSchema, ScalarExpr};

use super::Value;

mod composites;
pub(super) use composites::fold_composite_constructor;

pub(crate) fn retain_computed_integer(
    expression: ScalarExpr,
    parameter_index: Option<usize>,
) -> ScalarExpr {
    let ScalarExpr::Literal(Value::Int(value)) = expression else {
        return expression;
    };
    let ty = match uqa_sql::expr::integer_width_for_literal(value) {
        uqa_sql::expr::IntegerWidth::SmallInt => ColumnType::SmallInteger,
        uqa_sql::expr::IntegerWidth::Integer => ColumnType::Integer,
        uqa_sql::expr::IntegerWidth::BigInt => ColumnType::BigInteger,
    };
    ScalarExpr::TypedLiteral {
        composite_source: None,
        value: Value::Int(value),
        ty: ty.sql_name(),
        bound_type: Some(ty),
        parameter_index,
    }
}

pub(super) fn literal_value(expression: &ScalarExpr) -> Option<&Value> {
    match expression {
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => Some(value),
        _ => None,
    }
}

pub(super) fn is_coalesce(name: &str, binding: Option<&FunctionBinding>) -> bool {
    name.eq_ignore_ascii_case("coalesce") && binding.is_none_or(|binding| binding.builtin)
}

pub(in crate::optimizer) fn immutable_cast_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Array(element) => immutable_cast_type(element),
        ColumnType::SmallInteger
        | ColumnType::Integer
        | ColumnType::BigInteger
        | ColumnType::Oid
        | ColumnType::Xid
        | ColumnType::Boolean
        | ColumnType::Text
        | ColumnType::Name
        | ColumnType::Uuid
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::Real
        | ColumnType::DoublePrecision
        | ColumnType::Numeric { .. }
        | ColumnType::Json
        | ColumnType::JsonB
        | ColumnType::Bytea
        | ColumnType::InternalChar => true,
        _ => false,
    }
}

fn is_constant(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Literal(value) => {
            !uqa_sql::expr::composites::literal::contains_records(value)
                && !uqa_sql::expr::datums::contains_datum(value)
        }
        ScalarExpr::TypedLiteral {
            value,
            ty,
            bound_type,
            ..
        } => {
            !uqa_sql::expr::composites::literal::contains_records(value)
                && !uqa_sql::expr::datums::contains_datum(value)
                && (bound_type.is_some() || ColumnType::from_sql_name(ty).is_ok())
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().all(is_constant),
        ScalarExpr::Cast { expr, ty, .. } => {
            is_constant(expr)
                && ColumnType::from_sql_name(ty).is_ok_and(|target| {
                    immutable_cast_type(&target)
                        && scalar_type(expr, &RowSchema::default(), &[]).is_ok_and(|source| {
                            source.is_none_or(|source| {
                                uqa_sql::type_resolution::cast_volatility(&source, &target)
                                    == uqa_sql::ast::FunctionVolatility::Immutable
                            })
                        })
                })
        }
        // A named ROW constructor reads its current catalog descriptor even when every argument is a constant.
        ScalarExpr::CompositeRow { .. } => false,
        ScalarExpr::Binary { lhs, rhs, .. } => is_constant(lhs) && is_constant(rhs),
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. } => is_constant(inner),
        ScalarExpr::Between { expr, low, high } => {
            is_constant(expr) && is_constant(low) && is_constant(high)
        }
        ScalarExpr::InList { expr, list, .. } => is_constant(expr) && list.iter().all(is_constant),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_none_or(is_constant)
                && when
                    .iter()
                    .all(|(condition, value)| is_constant(condition) && is_constant(value))
                && else_branch.as_deref().is_none_or(is_constant)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } if is_coalesce(name, binding.as_ref()) => args.iter().all(is_constant),
        ScalarExpr::Func {
            order_syntax: _,
            name,
            binding: Some(binding),
            args,
            distinct: false,
            order_by,
            filter: None,
        } if binding.builtin
            && order_by.is_empty()
            && !uqa_sql::semantics::is_builtin_aggregate(expression)
            && !uqa_sql::semantics::sets::validation::builtin_returns_set(
                &uqa_sql::semantics::builtin_function_dispatch_name(&name.to_ascii_lowercase()),
            )
            && uqa_sql::semantics::volatility::builtin_function_volatility(
                name,
                Some(binding),
                args.len(),
            ) == uqa_sql::ast::FunctionVolatility::Immutable =>
        {
            args.iter().all(is_constant)
        }
        _ => false,
    }
}

#[cfg(test)]
pub(super) fn fold_literal_expression(
    expression: ScalarExpr,
    evaluate: crate::optimizer::ConstantEvaluator,
) -> Result<ScalarExpr, SQLError> {
    fold_authorized_literal(expression, evaluate, None, None)
}

pub(super) fn fold_authorized_literal(
    expression: ScalarExpr,
    evaluate: crate::optimizer::ConstantEvaluator,
    permissions: Option<&dyn uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution>,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
) -> Result<ScalarExpr, SQLError> {
    if matches!(&expression, ScalarExpr::Func { binding: Some(binding), .. }
        if matches!(binding.dispatch, Some(uqa_sql::ast::FunctionDispatch::NamedArgument | uqa_sql::ast::FunctionDispatch::VariadicArgument)))
    {
        // Argument markers carry syntax for their enclosing call; only that call evaluates them as arguments.
        return Ok(expression);
    }
    if let ScalarExpr::Cast { expr, ty, .. } = &expression {
        if literal_value(expr).is_some_and(|value| matches!(value, Value::Null)) {
            if let Ok(target) = ColumnType::from_sql_name(ty) {
                if !matches!(target, ColumnType::Domain { .. } | ColumnType::Named(_)) {
                    return Ok(ScalarExpr::TypedLiteral {
                        composite_source: None,
                        value: Value::Null,
                        ty: ty.clone(),
                        bound_type: Some(target),
                        parameter_index: None,
                    });
                }
            }
        }
    }
    if let Some(literal) = composite_constant_field(&expression, types) {
        return literal;
    }
    let strict_null = strict_null_expression(&expression);
    if literal_value(&expression).is_some() || (!strict_null && !is_constant(&expression)) {
        return Ok(expression);
    }
    let schema = RowSchema::default();
    let selected_type = match &expression {
        // An analyzed comparison has boolean output even when its discarded operand needs a live catalog to resolve its retained type name.
        ScalarExpr::Binary { .. } if strict_null => Some(ColumnType::Boolean),
        ScalarExpr::Func {
            binding: Some(binding),
            ..
        } if strict_null => {
            if let Some(error) = &binding.resolution_error {
                return Err(error.sql_error());
            }
            uqa_sql::type_resolution::fixed_builtin_return_type(binding)
        }
        _ => None,
    };
    let ty = match selected_type {
        Some(ty) => Some(ty),
        None => scalar_type(&expression, &schema, &[])?,
    };
    // Keep operator-selected casts before evaluation can replace the expression with a literal, including PostgreSQL unknown string inputs.
    let expression = uqa_sql::bind_type_introspection(expression, &schema, &[]);
    let value = if strict_null {
        // PostgreSQL simplifies a strict NULL call without looking up its function execution permission.
        Value::Null
    } else {
        if let (
            Some(permissions),
            ScalarExpr::Func {
                binding: Some(binding),
                ..
            },
        ) = (permissions, &expression)
        {
            permissions.require_execute(binding)?;
        }
        evaluate(&expression)?
    };
    let literal = ScalarExpr::Literal(value.clone());
    // Computed integers are not ORDER BY positions, and computed strings are not fresh unknown literals. Preserve their resolved types when replacing the expression.
    if !matches!(value, Value::Int(_) | Value::Str(_))
        && !matches!(expression, ScalarExpr::Cast { .. })
        && scalar_type(&literal, &schema, &[])? == ty
    {
        return Ok(literal);
    }
    Ok(match ty {
        Some(ty) => ScalarExpr::TypedLiteral {
            composite_source: None,
            value,
            ty: ty.sql_name(),
            bound_type: Some(ty),
            parameter_index: None,
        },
        None => literal,
    })
}

fn composite_constant_field(
    expression: &ScalarExpr,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
) -> Option<Result<ScalarExpr, SQLError>> {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = expression
    else {
        return None;
    };
    if binding.dispatch != Some(uqa_sql::ast::FunctionDispatch::FieldSelect) {
        return None;
    }
    let [base, ScalarExpr::Literal(Value::Str(name))] = args.as_slice() else {
        return None;
    };
    let base = match base {
        ScalarExpr::Cast { expr, ty, .. }
            if matches!(expr.as_ref(), ScalarExpr::TypedLiteral { ty: literal_type, bound_type, .. }
            if ty == literal_type || match (bound_type, types) {
                (Some(ColumnType::Composite(reference)), Some(types)) => types.resolve_catalog_column_type_name(ty).is_ok_and(|target| matches!(target, ColumnType::Composite(target) if target.oid == reference.oid)),
                _ => false,
            }) =>
        {
            expr
        }
        expression => expression,
    };
    let ScalarExpr::TypedLiteral {
        value,
        bound_type: Some(ColumnType::Composite(reference)),
        composite_source,
        ..
    } = base
    else {
        return None;
    };
    let resolved;
    let field = if let Some(field) = binding.composite_field.as_deref() {
        field
    } else {
        let descriptor =
            match uqa_sql::expr::composites::descriptor(types?.composite_types(), reference.oid) {
                Ok(descriptor) => descriptor,
                Err(error) => return Some(Err(error)),
            };
        let (_, attribute) = descriptor.attribute(name)?;
        resolved = uqa_sql::ast::CompositeFieldBinding {
            type_oid: reference.oid,
            number: attribute.number,
            result_type: attribute.ty.clone(),
            dropped: false,
            changed_type: None,
        };
        &resolved
    };
    if reference.oid != field.type_oid {
        return None;
    }
    let value = match value {
        Value::Null => Value::Null,
        Value::Record(fields) => {
            if fields.type_oid().is_some_and(|oid| oid != reference.oid) {
                return None;
            }
            if field.dropped {
                Value::Null
            } else {
                if let Err(error) = uqa_sql::expr::composites::validate_field_result(field) {
                    return Some(Err(error));
                }
                match uqa_sql::expr::datums::copy_constant_field(
                    &fields.iter().find(|(key, _)| key == name)?.1,
                    &field.result_type,
                ) {
                    Ok(value) => value,
                    Err(error) => return Some(Err(error)),
                }
            }
        }
        _ => return None,
    };
    if composite_source.is_some() {
        // Copying a selected constant can fail during planning. Keep the immutable source afterward so later descriptor changes still recompute its byte position.
        return None;
    }
    Some(Ok(ScalarExpr::TypedLiteral {
        composite_source: None,
        value,
        ty: field.result_type.catalog_name(),
        bound_type: Some(field.result_type.clone()),
        parameter_index: None,
    }))
}

/// Strict comparisons discard nonconstant siblings when an input is constant NULL, just as strict functions do. Compound predicates expose their individual comparisons before this pass.
fn strict_null_expression(expression: &ScalarExpr) -> bool {
    use uqa_sql::ast::BinaryOp;
    let null = |argument: &ScalarExpr| {
        literal_value(argument).is_some_and(|value| matches!(value, Value::Null))
    };
    match expression {
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            uqa_sql::expr::bound_scalar_function_strictness(name, binding.as_ref(), args.len())
                == Some(true)
                && args.iter().any(null)
        }
        ScalarExpr::Binary {
            op:
                BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual,
            lhs,
            rhs,
        } => null(lhs) || null(rhs),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
