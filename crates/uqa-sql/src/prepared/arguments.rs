//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static EXECUTE argument shape, assignment compatibility, and safe constant evaluation.

use super::error;
use crate::{ast::FunctionVolatility, plan::ExpressionPlan, ColumnType, SQLError, ScalarExpr};

pub struct ArgumentValidationContext<'a> {
    pub aggregates: &'a dyn crate::plan::AggregateClassifier,
    pub volatility: &'a dyn crate::semantics::volatility::VolatilityCatalog,
    pub cast_type: &'a dyn Fn(&str) -> Option<ColumnType>,
}

pub fn validate_assignment_type(
    index: usize,
    source: &ColumnType,
    target: &ColumnType,
) -> Result<(), SQLError> {
    if !crate::assignment_type_compatible(source, target) {
        return Err(error(
            "42804",
            format!(
                "parameter ${} of type {} cannot be coerced to the expected type {}",
                index + 1,
                source.sql_name(),
                target.sql_name()
            ),
        ));
    }
    Ok(())
}

pub fn parameter_base_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    ty
}

pub fn contains_domain(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Domain { .. } => true,
        ColumnType::Array(element) => contains_domain(element),
        _ => false,
    }
}

pub fn immutable_argument(
    context: &ArgumentValidationContext<'_>,
    expression: &ScalarExpr,
) -> bool {
    let mut immutable = true;
    expression.visit(&mut |expression| match expression {
        ScalarExpr::Param(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::Position(_) => immutable = false,
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            immutable &= crate::semantics::volatility::function_volatility_with_binding(
                context.volatility,
                name,
                binding.as_ref(),
                args.len(),
            ) == FunctionVolatility::Immutable;
        }
        ScalarExpr::Cast { ty, .. } => {
            immutable &= !(context.cast_type)(ty)
                .as_ref()
                .is_some_and(contains_domain);
        }
        _ => {}
    });
    immutable
}

pub fn validate_argument(
    aggregates: &dyn crate::plan::AggregateClassifier,
    argument: &ExpressionPlan,
) -> Result<(), SQLError> {
    if !argument.subqueries.is_empty() {
        return Err(error("0A000", "cannot use subquery in EXECUTE parameter"));
    }
    if argument.scalar.contains_window() {
        return Err(error(
            "42P20",
            "window functions are not allowed in EXECUTE parameters",
        ));
    }
    if crate::semantics::aggregates::contains_aggregate(aggregates, &argument.scalar) {
        return Err(error(
            "42803",
            "aggregate functions are not allowed in EXECUTE parameters",
        ));
    }
    Ok(())
}
