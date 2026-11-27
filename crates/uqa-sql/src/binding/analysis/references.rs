//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive scalar and subquery reference validation.

use super::super::{SQLError, SQLParam, ScalarExpr};
use super::functions::{
    validate_qualified_column, validate_unqualified_column, validate_window_function,
};
use crate::ast::RuleEvent;
use crate::ir::children::Scope;
use crate::routines::RoutineResolution;
use crate::{FunctionTypeResolver, RowSchema};

pub(super) fn validate_expression(
    rule_event: Option<RuleEvent>,
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    schema: &RowSchema,
    fallback: Option<&RowSchema>,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<(), SQLError> {
    validate_expression_inner(
        rule_event, routines, expression, schema, fallback, params, resolver,
    )
    .map_err(|error| error.at_position(expression.reference_position()))
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
fn validate_expression_inner(
    rule_event: Option<RuleEvent>,
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    schema: &RowSchema,
    fallback: Option<&RowSchema>,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<(), SQLError> {
    let validate_child = |child: &ScalarExpr| {
        validate_expression(rule_event, routines, child, schema, None, params, resolver)
    };
    match expression {
        ScalarExpr::Column(column) => validate_unqualified_column(schema, fallback, column),
        ScalarExpr::Position(position) => {
            if *position < schema.len() {
                Ok(())
            } else {
                Err(SQLError::UnknownColumn((position + 1).to_string()))
            }
        }
        ScalarExpr::InternalColumn(column) => {
            if schema.internal_slot(*column).is_some()
                || fallback.is_some_and(|fallback| fallback.internal_slot(*column).is_some())
            {
                Ok(())
            } else {
                Err(SQLError::Internal(format!(
                    "internal relation attribute {column:?} is outside the bound row scope"
                )))
            }
        }
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            if !schema.has_qualifier(qualifier)
                && !fallback.is_some_and(|schema| schema.has_qualifier(qualifier))
                && rule_event.is_some_and(|event| {
                    crate::catalog::events::validation::unavailable_rule_row(event, qualifier)
                })
            {
                return Err(
                    crate::semantics::rules::action_binding::invalid_rule_relation_reference(
                        qualifier,
                    ),
                );
            }
            validate_qualified_column(schema, fallback, qualifier, column)
        }
        ScalarExpr::QualifiedStar(qualifier) => {
            if schema.has_qualifier(qualifier) {
                schema.require_lateral_qualifier(qualifier)?;
            } else if let Some(fallback) = fallback {
                fallback.require_lateral_qualifier(qualifier)?;
            }
            if schema.has_qualifier(qualifier)
                || fallback.is_some_and(|fallback| fallback.has_qualifier(qualifier))
            {
                Ok(())
            } else if rule_event.is_some_and(|event| {
                crate::catalog::events::validation::unavailable_rule_row(event, qualifier)
            }) {
                Err(
                    crate::semantics::rules::action_binding::invalid_rule_relation_reference(
                        qualifier,
                    ),
                )
            } else {
                for candidate in std::iter::once(schema).chain(fallback) {
                    if let Some(error) = candidate.unavailable_qualifier_error(qualifier) {
                        return Err(error);
                    }
                }
                Err(SQLError::UnknownTable(qualifier.to_string()))
            }
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            filter,
            ..
        } => {
            for (argument_index, argument) in args.iter().enumerate() {
                if crate::semantics::is_semantic_field_argument(name, args, argument_index)? {
                    continue;
                }
                validate_child(argument)?;
            }
            if let Some(crate::ast::FunctionDispatch::BooleanTest(test)) =
                binding.as_ref().and_then(|binding| binding.dispatch)
            {
                let [argument] = args.as_slice() else {
                    return Err(SQLError::Internal("Boolean test takes one input".into()));
                };
                require_boolean_condition(argument, test.label(), schema, params, resolver)?;
                return require_scalar_boolean_test(routines, argument, schema, params, test);
            }
            for order in order_by {
                validate_child(&order.expr)?;
            }
            if let Some(filter) = filter.as_deref() {
                validate_child(filter)?;
                require_boolean_condition(filter, "FILTER", schema, params, resolver)?;
            }
            super::validate_call(routines, expression, schema, params, resolver)
        }
        ScalarExpr::WindowCall {
            name,
            args,
            filter,
            modifiers,
            ..
        } => {
            for child in expression.children(Scope::Complete) {
                validate_child(child)?;
                if filter
                    .as_deref()
                    .is_some_and(|filter| std::ptr::eq(child, filter))
                {
                    require_boolean_condition(child, "FILTER", schema, params, resolver)?;
                }
            }
            validate_window_function(
                routines,
                name,
                (args, filter.is_some(), *modifiers),
                schema,
                params,
                resolver,
            )
        }
        ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            let construct = if matches!(expression, ScalarExpr::And(_)) {
                "AND"
            } else {
                "OR"
            };
            for item in items {
                validate_child(item)?;
                require_boolean_condition(item, construct, schema, params, resolver)?;
            }
            Ok(())
        }
        ScalarExpr::Not(inner) => {
            validate_child(inner)?;
            require_boolean_condition(inner, "NOT", schema, params, resolver)
        }
        ScalarExpr::Array(_)
        | ScalarExpr::Row(_)
        | ScalarExpr::CompositeRow { .. }
        | ScalarExpr::Binary { .. }
        | ScalarExpr::UnaryMinus(_)
        | ScalarExpr::IsNull { .. }
        | ScalarExpr::Cast { .. }
        | ScalarExpr::Collated { .. }
        | ScalarExpr::Between { .. }
        | ScalarExpr::InList { .. } => expression
            .children(Scope::Complete)
            .try_for_each(validate_child),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_deref() {
                validate_child(base)?;
            }
            for (condition, value) in when {
                validate_child(condition)?;
                // A searched CASE tests each condition; a simple CASE compares the operand with each value.
                if base.is_none() {
                    require_boolean_condition(condition, "CASE/WHEN", schema, params, resolver)?;
                }
                validate_child(value)?;
            }
            if let Some(else_branch) = else_branch.as_deref() {
                validate_child(else_branch)?;
            }
            Ok(())
        }
        ScalarExpr::ScalarSubquery(index)
        | ScalarExpr::Exists {
            subquery: index, ..
        } => resolver
            .resolve_scalar_subquery_type(*index, schema, params)
            .map(drop),
        ScalarExpr::InSubquery { expr, subquery, .. } => {
            validate_child(expr)?;
            resolver
                .resolve_scalar_subquery_type(*subquery, schema, params)
                .map(drop)
        }
        ScalarExpr::Param(number) => number
            .checked_sub(1)
            .and_then(|index| params.get(index))
            .map(|_| ())
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42P02".into(),
                message: format!("there is no parameter ${number}"),
            }),
        ScalarExpr::Star
        | ScalarExpr::Default
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_) => Ok(()),
    }
}

/// `coerce_to_boolean`: the condition of `construct` must be boolean. An `unknown` literal is read as boolean input, and the engine's retrieval predicates, which the planner turns into index searches, qualify as conditions.
pub(super) fn require_boolean_condition(
    condition: &ScalarExpr,
    construct: &str,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<(), SQLError> {
    if let ScalarExpr::Literal(uqa_core::Value::Str(text)) = condition {
        return crate::expr::parse_boolean_input(text)
            .map(drop)
            .ok_or_else(|| crate::expr::invalid_boolean_input(text));
    }
    if let ScalarExpr::Func { name, .. } = condition {
        if crate::registry::is_registered(&crate::semantics::builtin_function_dispatch_name(name)) {
            return Ok(());
        }
    }
    let ty = crate::scalar_type_with_resolver(condition, schema, params, resolver)?;
    let Some(ty) = crate::effective_overload_argument_type_with_params(condition, ty, params)
    else {
        return Ok(());
    };
    crate::type_resolution::boolean::require_argument_type(Some(&ty), construct, Some(resolver))
}

fn require_scalar_boolean_test(
    routines: &dyn RoutineResolution,
    argument: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    test: crate::ast::BooleanTest,
) -> Result<(), SQLError> {
    if crate::semantics::sets::placement_scope::placing_set_calls()
        && crate::semantics::sets::validation::expression_may_return_set(
            routines, routines, argument, schema, params,
        )?
    {
        Err(crate::semantics::sets::placement::set_argument_error(
            test.label(),
        ))
    } else {
        Ok(())
    }
}
