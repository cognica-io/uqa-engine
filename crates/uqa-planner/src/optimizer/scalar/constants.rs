//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluation of immutable constants without erasing their declared SQL type.

use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::ast::{BinaryOp, ColumnType, FunctionBinding, FunctionVolatility};
use uqa_sql::SQLError;
use uqa_sql::{scalar_type, FunctionTypeResolver, RowSchema, ScalarExpr};

use super::Value;

mod composites;
pub(super) use composites::fold_composite_constructor;

/// Eliminating a CASE, COALESCE or function call must not turn its record result into ROW comparison syntax.
pub(super) fn retain_record_result(expression: ScalarExpr, row_syntax: bool) -> ScalarExpr {
    if !row_syntax && matches!(expression, ScalarExpr::Row(_)) {
        ScalarExpr::Cast {
            implicit: true,
            expr: Box::new(expression),
            ty: ColumnType::Record.catalog_name(),
        }
    } else {
        expression
    }
}

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
        collation_oid: None,
        composite_source: None,
        value: Value::Int(value),
        ty: ty.catalog_name(),
        bound_type: Some(ty),
        parameter_index,
    }
}

pub(super) enum LiteralValue<'a> {
    Borrowed(&'a Value),
    Owned(Produced<Value>),
}

impl std::ops::Deref for LiteralValue<'_> {
    type Target = Value;
    fn deref(&self) -> &Value {
        match self {
            Self::Borrowed(value) => value,
            Self::Owned(value) => value,
        }
    }
}

#[cfg(test)]
pub(super) fn literal_value(expression: &ScalarExpr) -> Result<Option<LiteralValue<'_>>, SQLError> {
    literal_value_with_control(expression, &ProductionControl::uncontrolled())
}

pub(super) fn literal_value_with_control<'a>(
    expression: &'a ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<Option<LiteralValue<'a>>, SQLError> {
    control.check()?;
    Ok(match expression {
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => {
            Some(LiteralValue::Borrowed(value))
        }
        ScalarExpr::RetainedLiteral(literal) => {
            uqa_sql::expr::check_statement_cancellation()?;
            Some(match literal.value.resident_value() {
                Some(value) => LiteralValue::Borrowed(value),
                None => LiteralValue::Owned(literal.value.read_with_control(control)?),
            })
        }
        ScalarExpr::Collated { expr, .. } => return literal_value_with_control(expr, control),
        _ => None,
    })
}

pub(super) fn retain_large_literal(
    expression: ScalarExpr,
    storage: Option<&dyn uqa_sql::ir::ConstantValueStorage>,
) -> Result<ScalarExpr, SQLError> {
    let Some(storage) = storage else {
        return Ok(expression);
    };
    let (value, selected) = match &expression {
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
    // Inline payload is bounded by one IR node. Large VALUES datums share the statement's byte-bounded owner instead of accumulating in every plan copy.
    let cancellation = uqa_sql::expr::statement_cancellation().unwrap_or_default();
    let bytes = value.retained_payload_bytes(
        &uqa_core::memory::MemoryBudget::new(usize::MAX),
        &cancellation,
    )?;
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
        inferred = scalar_type(&expression, &RowSchema::default(), &[])?;
        let Some(ty) = inferred.as_ref() else {
            return Ok(expression);
        };
        ty
    };
    if uqa_sql::type_resolution::pg_catalog_builtin_type(&ty.catalog_name()).as_ref() != Some(ty) {
        return Ok(expression);
    }
    let ty = ty.clone();
    let (value, declaration, collation_oid) = match expression {
        ScalarExpr::TypedLiteral {
            value,
            ty,
            collation_oid,
            ..
        } => (value, Some(ty), collation_oid),
        ScalarExpr::Literal(value) => (value, None, None),
        _ => unreachable!("selected literal"),
    };
    Ok(ScalarExpr::RetainedLiteral(
        uqa_sql::ir::RetainedScalarLiteral::new(
            storage.retain(value)?,
            Some(ty),
            declaration,
            collation_oid,
        ),
    ))
}

pub(super) fn is_coalesce(name: &str, binding: Option<&FunctionBinding>) -> bool {
    name.eq_ignore_ascii_case("coalesce") && binding.is_none_or(|binding| binding.builtin)
}

fn constant_cast_type(
    name: &str,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
) -> Option<ColumnType> {
    if let Some(types) = types {
        return types.resolve_catalog_column_type_name(name).ok();
    }
    uqa_sql::type_resolution::pg_catalog_builtin_type(name)
}

fn is_constant(
    expression: &ScalarExpr,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> bool {
    // Keep an explicit clause's coercibility; folding its operand must not turn the clause into an implicit constant.
    if uqa_sql::catalog::collations::expression::scalar_clause(expression)
        .is_ok_and(|clause| clause.is_some())
    {
        return false;
    }
    let constant = |expression: &ScalarExpr| is_constant(expression, types, resolver);
    match expression {
        ScalarExpr::RetainedLiteral(_) => true,
        ScalarExpr::Collated { expr, .. } => constant(expr),
        ScalarExpr::Literal(value) => immutable_literal(value),
        ScalarExpr::TypedLiteral {
            value,
            ty,
            bound_type,
            ..
        } => {
            immutable_literal(value)
                && (bound_type.is_some() || constant_cast_type(ty, types).is_some())
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().all(constant),
        ScalarExpr::Cast { expr, ty, .. } => {
            constant(expr)
                && constant_cast_type(ty, types).is_some_and(|target| {
                    uqa_sql::type_resolution::immutable_cast_type(&target)
                        && constant_scalar_type(expr, resolver).is_ok_and(|source| {
                            source.is_none_or(|source| {
                                uqa_sql::type_resolution::cast_volatility(&source, &target)
                                    == uqa_sql::ast::FunctionVolatility::Immutable
                            })
                        })
                })
        }
        // A named ROW constructor reads its current catalog descriptor even when every argument is a constant.
        ScalarExpr::CompositeRow { .. } => false,
        ScalarExpr::Binary { op, lhs, rhs } => {
            constant(lhs) && constant(rhs) && immutable_binary(*op, lhs, rhs, resolver)
        }
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. } => constant(inner),
        ScalarExpr::Between { expr, low, high } => {
            constant(expr)
                && constant(low)
                && constant(high)
                && immutable_binary(BinaryOp::LessEqual, low, expr, resolver)
                && immutable_binary(BinaryOp::LessEqual, expr, high, resolver)
        }
        ScalarExpr::InList { expr, list, .. } => {
            constant(expr)
                && list.iter().all(|item| {
                    constant(item) && immutable_binary(BinaryOp::Equal, expr, item, resolver)
                })
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_none_or(constant)
                && when
                    .iter()
                    .all(|(condition, value)| constant(condition) && constant(value))
                && else_branch.as_deref().is_none_or(constant)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } if is_coalesce(name, binding.as_ref()) => args.iter().all(constant),
        ScalarExpr::Func {
            order_syntax: _,
            name,
            binding: Some(binding),
            args,
            distinct: false,
            order_by,
            filter: None,
        } if binding.builtin
            && !uqa_sql::semantics::function_has_implicit_row_input(binding)
            && order_by.is_empty()
            && !uqa_sql::semantics::is_builtin_aggregate(expression)
            && !uqa_sql::semantics::sets::validation::builtin_returns_set(
                &uqa_sql::semantics::builtin_function_dispatch_name(name),
            )
            && uqa_sql::semantics::volatility::builtin_function_volatility(
                name,
                Some(binding),
                args.len(),
            ) == uqa_sql::ast::FunctionVolatility::Immutable =>
        {
            args.iter().all(constant)
        }
        _ => false,
    }
}

fn immutable_literal(value: &Value) -> bool {
    !uqa_sql::expr::composites::literal::contains_records(value)
        && !uqa_sql::expr::datums::contains_datum(value)
}

fn constant_scalar_type(
    expression: &ScalarExpr,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    let schema = RowSchema::default();
    match resolver {
        Some(resolver) => uqa_sql::scalar_type_with_resolver(expression, &schema, &[], resolver),
        None => scalar_type(expression, &schema, &[]),
    }
}

fn immutable_binary(
    op: BinaryOp,
    left: &ScalarExpr,
    right: &ScalarExpr,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> bool {
    let [Ok(left), Ok(right)] = [left, right].map(|expr| constant_scalar_type(expr, resolver))
    else {
        // The ordinary typed fold reports analysis errors before evaluating a constant.
        return true;
    };
    uqa_sql::type_resolution::binary_operator_volatility(op, left.as_ref(), right.as_ref())
        .map_or(true, |volatility| {
            volatility == FunctionVolatility::Immutable
        })
}

#[cfg(test)]
pub(super) fn fold_literal_expression(
    expression: ScalarExpr,
    evaluate: fn(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<ScalarExpr, SQLError> {
    fold_authorized_literal(expression, evaluate, None, None, None)
}

#[cfg(test)]
pub(super) fn fold_authorized_literal(
    expression: ScalarExpr,
    evaluate: fn(&ScalarExpr) -> Result<Value, SQLError>,
    permissions: Option<&dyn uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution>,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<ScalarExpr, SQLError> {
    let config =
        crate::OptimizerConfig::new(|_, _, _| unreachable!("test adapter supplies evaluation"));
    fold_selected_literal(
        expression,
        |expression, control| Ok(control.finish(evaluate(expression)?, None)?),
        permissions,
        types,
        resolver,
        None,
        &config,
    )
}

pub(super) fn fold_selected_literal(
    expression: ScalarExpr,
    evaluate: impl Fn(&ScalarExpr, &ProductionControl<'_>) -> Result<Produced<Value>, SQLError>,
    permissions: Option<&dyn uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution>,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
    resolver: Option<&dyn FunctionTypeResolver>,
    result_type: Option<&ColumnType>,
    config: &crate::OptimizerConfig<'_>,
) -> Result<ScalarExpr, SQLError> {
    if matches!(&expression, ScalarExpr::Func { binding: Some(binding), .. }
        if matches!(binding.dispatch, Some(uqa_sql::ast::FunctionDispatch::NamedArgument | uqa_sql::ast::FunctionDispatch::VariadicArgument)))
    {
        // Argument markers carry syntax for their enclosing call; only that call evaluates them as arguments.
        return Ok(expression);
    }
    if let ScalarExpr::Cast { expr, ty, .. } = &expression {
        if literal_value_with_control(expr, &config.production)?
            .as_deref()
            .is_some_and(|value| matches!(value, Value::Null))
        {
            if let Some(target) = constant_cast_type(ty, types) {
                if !matches!(target, ColumnType::Domain { .. } | ColumnType::Named(_)) {
                    return Ok(ScalarExpr::TypedLiteral {
                        collation_oid: None,
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
    let strict_null = strict_null_expression(&expression, &config.production)?;
    if matches!(expression, ScalarExpr::RetainedLiteral(_))
        || literal_value_with_control(&expression, &config.production)?.is_some()
        || (!strict_null && !is_constant(&expression, types, resolver))
        || (strict_null && arithmetic_needs_result_type(&expression, result_type))
    {
        return Ok(expression);
    }
    let schema = RowSchema::default();
    let selected_type = strict_null_result_type(&expression, strict_null)?;
    let ty = match result_type.cloned().or(selected_type) {
        Some(ty) => Some(ty),
        None => constant_scalar_type(&expression, resolver)?,
    };
    // Keep operator-selected casts before evaluation can replace the expression with a literal, including PostgreSQL unknown string inputs.
    let expression = match resolver {
        Some(resolver) => {
            uqa_sql::bind_type_introspection_with_resolver(expression, &schema, &[], resolver)
        }
        None => uqa_sql::bind_type_introspection(expression, &schema, &[]),
    };
    let value = if strict_null {
        // PostgreSQL simplifies a strict NULL call without looking up its function execution permission.
        config
            .production
            .finish(Value::Null, config.production.empty_reservation())?
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
        evaluate(&expression, &config.production)?
    };
    let (value, memory) = value.into_parts();
    let literal = ScalarExpr::Literal(value);
    // Computed integers are not ORDER BY positions, and computed strings are not fresh unknown literals. Preserve their resolved types when replacing the expression.
    let untyped = !matches!(literal, ScalarExpr::Literal(Value::Int(_) | Value::Str(_)))
        && !matches!(expression, ScalarExpr::Cast { .. })
        && constant_scalar_type(&literal, resolver)? == ty;
    let ScalarExpr::Literal(value) = literal else {
        unreachable!("evaluated literal")
    };
    let value = config.production.finish(value, memory)?;
    if config.production.budget().is_some() && value.reserved_bytes() > 0 {
        return retain_produced_literal(value, ty, !untyped, config);
    }
    // The ordinary result, or an inline scalar with an empty lease, owns no controlled allocation after this move.
    let (value, memory) = value.into_parts();
    drop(memory);
    if untyped {
        return Ok(ScalarExpr::Literal(value));
    }
    Ok(match ty {
        Some(ty) => ScalarExpr::TypedLiteral {
            collation_oid: None,
            composite_source: None,
            value,
            ty: ty.catalog_name(),
            bound_type: Some(ty),
            parameter_index: None,
        },
        None => ScalarExpr::Literal(value),
    })
}

fn retain_produced_literal(
    value: Produced<Value>,
    ty: Option<ColumnType>,
    declared: bool,
    config: &crate::OptimizerConfig<'_>,
) -> Result<ScalarExpr, SQLError> {
    use uqa_sql::ir::{RetainedConstantValue, RetainedScalarLiteral};
    let control = &config.production;
    let declaration = if declared {
        ty.as_ref()
            .map(|ty| ty.catalog_name_with_control(control))
            .transpose()?
    } else {
        None
    };
    let retained_type = ty
        .as_ref()
        .map(|ty| ty.clone_with_control(control))
        .transpose()?;
    let storage = config.constant_storage.as_deref().filter(|_| {
        config.retain_values_constants
            && value.reserved_bytes() > size_of::<ScalarExpr>()
            && !uqa_sql::expr::composites::literal::contains_records(&value)
            && !uqa_sql::expr::datums::contains_datum(&value)
            && ty.as_ref().is_some_and(|ty| {
                uqa_sql::type_resolution::pg_catalog_builtin_type(&ty.catalog_name()).as_ref()
                    == Some(ty)
            })
    });
    let value = if let Some(storage) = storage {
        // The spill owner publishes the complete value while its production lease is still live. Only then can the transient decoded payload and its lease be released.
        let (value, memory) = value.into_parts();
        let value = storage.retain(value)?;
        drop(memory);
        value
    } else {
        RetainedConstantValue::from_produced(value, control)?
    };
    Ok(ScalarExpr::RetainedLiteral(
        RetainedScalarLiteral::with_control(value, retained_type, declaration, None, control)?,
    ))
}

fn strict_null_result_type(
    expression: &ScalarExpr,
    strict_null: bool,
) -> Result<Option<ColumnType>, SQLError> {
    Ok(match expression {
        // An analyzed comparison has boolean output even when its discarded operand needs a live catalog to resolve its retained type name.
        ScalarExpr::Binary {
            op:
                BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual,
            ..
        } if strict_null => Some(ColumnType::Boolean),
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
    })
}

fn arithmetic_needs_result_type(expression: &ScalarExpr, result_type: Option<&ColumnType>) -> bool {
    if result_type.is_some()
        || !matches!(
            expression,
            ScalarExpr::Binary {
                op: BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide,
                ..
            }
        )
    {
        return false;
    }
    // A raw optimizer caller has no row or parameter schema. Unknown runtime inputs must not select an operator as if they were SQL unknown literals; analyzed callers retain the result explicitly.
    let mut unknown = false;
    expression.visit(&mut |node| {
        unknown |= matches!(
            node,
            ScalarExpr::Column(_)
                | ScalarExpr::QualifiedColumn { .. }
                | ScalarExpr::Position(_)
                | ScalarExpr::InternalColumn(_)
                | ScalarExpr::Param(_)
        );
    });
    unknown
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
    let field = if let Some(field) = binding.composite_field() {
        field
    } else {
        let descriptor =
            match uqa_sql::expr::composites::descriptor(types?.composite_types(), reference.oid) {
                Ok(descriptor) => descriptor,
                Err(error) => return Some(Err(error)),
            };
        let (_, attribute) = descriptor.attribute(name)?;
        resolved = uqa_sql::ast::CompositeFieldBinding {
            collation_oid: Some(attribute.collation_oid()),
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
    let value = match composites::constant_field(value, name, field, types)? {
        Ok(value) => value,
        Err(error) => return Some(Err(error)),
    };
    if composite_source.is_some() {
        // Copying a selected constant can fail during planning. Keep the immutable source afterward so later descriptor changes still recompute its byte position.
        return None;
    }
    Some(Ok(ScalarExpr::TypedLiteral {
        collation_oid: field.collation_oid,
        composite_source: None,
        value,
        ty: field.result_type.catalog_name(),
        bound_type: Some(field.result_type.clone()),
        parameter_index: None,
    }))
}

/// Strict operators discard nonconstant siblings when an input is constant NULL, just as strict functions do. Compound predicates expose their individual comparisons before this pass.
fn strict_null_expression(
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    use uqa_sql::ast::BinaryOp;
    let null = |argument: &ScalarExpr| {
        Ok::<_, SQLError>(
            literal_value_with_control(argument, control)?
                .as_deref()
                .is_some_and(|value| matches!(value, Value::Null)),
        )
    };
    Ok(match expression {
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            if name.eq_ignore_ascii_case("nullif")
                && binding.as_ref().is_none_or(|binding| binding.builtin)
                && args.len() == 2
                && null(&args[0])?
            {
                return Ok(true);
            }
            // A strict set-returning call with a NULL input returns no rows, not one NULL value.
            uqa_sql::expr::bound_scalar_function_strictness(name, binding.as_ref(), args.len())
                == Some(true)
                && !uqa_sql::semantics::sets::validation::builtin_returns_set(
                    &uqa_sql::semantics::builtin_function_dispatch_name(name),
                )
                && args.iter().try_fold(false, |found, argument| {
                    Ok::<_, SQLError>(found || null(argument)?)
                })?
        }
        ScalarExpr::Binary {
            op:
                BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual
                | BinaryOp::Add
                | BinaryOp::Subtract
                | BinaryOp::Multiply
                | BinaryOp::Divide,
            lhs,
            rhs,
        } => null(lhs)? || null(rhs)?,
        _ => false,
    })
}

/// `PostgreSQL` evaluates a whole call when a strict input is a NULL constant or every input is constant; otherwise it inlines the SQL-language `substring(text, text, text)` and folds a constant `similar_to_escape` in its body. The planner keeps the call and checks only that translation, so its errors still surface before any row is read.
pub(super) fn check_inlined_similar_substring(
    expression: &ScalarExpr,
    permissions: Option<&dyn uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution>,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<(), SQLError> {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        distinct: false,
        order_by,
        filter: None,
        ..
    } = expression
    else {
        return Ok(());
    };
    if !binding.builtin
        || uqa_sql::type_resolution::selected_builtin_dispatch(binding)
            != Some(uqa_sql::ast::FunctionDispatch::SimilarSubstring)
        || !order_by.is_empty()
    {
        return Ok(());
    }
    let [_, pattern, escape] = args.as_slice() else {
        return Ok(());
    };
    let (Some(pattern), Some(escape)) = (
        literal_value_with_control(pattern, control)?,
        literal_value_with_control(escape, control)?,
    ) else {
        return Ok(());
    };
    if strict_null_expression(expression, control)? || is_constant(expression, types, resolver) {
        return Ok(());
    }
    // `inline_function` declines a routine the caller cannot execute; execution then reports the privilege error.
    if permissions.is_some_and(|permissions| permissions.require_execute(binding).is_err()) {
        return Ok(());
    }
    uqa_sql::expr::check_constant_similar_translation(&pattern, &escape, control)
}

#[cfg(test)]
mod tests;
