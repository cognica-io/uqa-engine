//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluation of immutable constants without erasing their declared SQL type.

use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::ast::{BinaryOp, ColumnType, FunctionBinding, FunctionVolatility};
#[cfg(test)]
use uqa_sql::scalar_type;
use uqa_sql::SQLError;
use uqa_sql::{FunctionTypeResolver, RowSchema, ScalarExpr};

use super::Value;

mod composites;
mod domains;
mod eligibility;
pub(in crate::optimizer) mod metadata;
pub(super) mod owned;
pub(super) use composites::owned::fold_constructor as fold_owned_composite_constructor;
pub(crate) fn retain_computed_integer_owned(
    expression: uqa_sql::plan::OwnedScalarExpr,
    parameter_index: Option<usize>,
    control: &ProductionControl<'_>,
) -> Result<uqa_sql::plan::OwnedScalarExpr, SQLError> {
    metadata::retain_computed_integer(expression, parameter_index, control)
}
pub(crate) fn retain_computed_integer(
    expression: ScalarExpr,
    parameter_index: Option<usize>,
) -> ScalarExpr {
    let control = ProductionControl::uncontrolled();
    metadata::retain_computed_integer(
        uqa_sql::plan::OwnedScalarExpr::uncontrolled(expression),
        parameter_index,
        &control,
    )
    .expect("ordinary computed integer metadata")
    .finish(&control)
    .expect("ordinary computed integer result")
    .into_uncontrolled()
    .expect("ordinary computed integer owner")
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

pub(super) fn is_coalesce(name: &str, binding: Option<&FunctionBinding>) -> bool {
    name.eq_ignore_ascii_case("coalesce") && binding.is_none_or(|binding| binding.builtin)
}

#[cfg(test)]
fn is_constant(
    expression: &ScalarExpr,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> bool {
    eligibility::is_constant(
        expression,
        types,
        resolver,
        &ProductionControl::uncontrolled(),
    )
    .expect("ordinary constant eligibility")
}

fn immutable_literal(value: &Value) -> bool {
    !uqa_sql::expr::composites::literal::contains_records(value)
        && !uqa_sql::expr::datums::contains_datum(value)
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

#[cfg(test)]
pub(super) fn fold_selected_literal(
    expression: ScalarExpr,
    evaluate: impl Fn(&ScalarExpr, &ProductionControl<'_>) -> Result<Produced<Value>, SQLError>,
    permissions: Option<&dyn uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution>,
    types: Option<&dyn uqa_sql::routines::declaration::RoutineTypeCatalog>,
    resolver: Option<&dyn FunctionTypeResolver>,
    result_type: Option<&ColumnType>,
    config: &crate::OptimizerConfig<'_>,
) -> Result<ScalarExpr, SQLError> {
    let production = super::ScalarProduction::caller(config);
    let expression = uqa_sql::plan::OwnedScalarExpr::uncontrolled(expression);
    let output = owned::fold_selected(
        expression,
        result_type,
        production,
        permissions,
        types,
        resolver,
        evaluate,
    )?;
    Ok(output
        .finish(&production.storage)?
        .into_uncontrolled()
        .expect("caller-owned scalar"))
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
            let normalized = eligibility::normalized_name(name, control)?;
            if uqa_sql::expr::bound_scalar_function_strictness(
                &normalized,
                binding.as_ref(),
                args.len(),
            ) != Some(true)
            {
                return Ok(false);
            }
            !uqa_sql::semantics::sets::validation::builtin_returns_set(
                &uqa_sql::semantics::builtin_function_dispatch_name(&normalized),
            ) && args.iter().try_fold(false, |found, argument| {
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
    if strict_null_expression(expression, control)?
        || eligibility::is_constant(expression, types, resolver, control)?
    {
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
