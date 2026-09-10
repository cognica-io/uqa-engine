//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Determine nullable join sides from SQL predicates and strict function metadata.

use crate::semantics::{join_alias_input_schemas, resolve_join_using};
use crate::{
    binding::{bind_source_plan_schema, context::BindingContext},
    plan::SourcePlan,
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::Value;

pub fn reduce_null_rejected_outer_joins_to_fixpoint(
    routines: &dyn RoutineResolution,
    source: &mut SourcePlan,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    context: &BindingContext<'_>,
) -> Result<(), SQLError> {
    loop {
        let mut qualifications = predicate.iter().map(|expr| (*expr).clone()).collect();
        collect_inner_join_qualifications(source, &mut qualifications);
        let mut changed = false;
        for qualification in &qualifications {
            changed |= reduce_null_rejected_outer_joins(
                routines,
                source,
                qualification,
                params,
                context,
                None,
            )?;
        }
        if !changed {
            return Ok(());
        }
    }
}

fn collect_inner_join_qualifications(source: &SourcePlan, output: &mut Vec<ScalarExpr>) {
    let SourcePlan::Join {
        left,
        right,
        kind,
        on,
        ..
    } = source
    else {
        return;
    };
    if !matches!(
        kind,
        crate::ast::JoinKind::Inner | crate::ast::JoinKind::Cross
    ) {
        return;
    }
    if let Some(on) = on {
        output.push(on.clone());
    }
    collect_inner_join_qualifications(left, output);
    collect_inner_join_qualifications(right, output);
}

fn reduce_null_rejected_outer_joins(
    routines: &dyn RoutineResolution,
    source: &mut SourcePlan,
    predicate: &ScalarExpr,
    params: &[SQLParam],
    context: &BindingContext<'_>,
    outer: Option<&RowSchema>,
) -> Result<bool, SQLError> {
    let SourcePlan::Join {
        left,
        right,
        kind,
        using,
        natural,
        alias,
        column_aliases,
        lateral,
        ..
    } = source
    else {
        return Ok(false);
    };
    let left_schema = bind_source_plan_schema(routines, left, params, context, outer)?;
    let implicit_lateral_function = matches!(
        right.as_ref(),
        SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
    );
    let right_scope =
        (*lateral || implicit_lateral_function).then(|| overlay_outer_schema(&left_schema, outer));
    let right_outer = right_scope.as_ref().or(outer);
    let right_schema = bind_source_plan_schema(routines, right, params, context, right_outer)?;
    let resolved_using = resolve_join_using(using.as_ref(), *natural, &left_schema, &right_schema)?;
    let (left_predicate_schema, right_predicate_schema) = match alias.as_deref() {
        Some(alias) => join_alias_input_schemas(
            *kind,
            &left_schema,
            &right_schema,
            resolved_using.as_ref(),
            alias,
            column_aliases,
        )?,
        None => (left_schema.clone(), right_schema.clone()),
    };
    let rejects_left = predicate_rejects_null_extended_side(
        routines,
        predicate,
        &left_predicate_schema,
        &right_predicate_schema,
        params,
    )?;
    let rejects_right = predicate_rejects_null_extended_side(
        routines,
        predicate,
        &right_predicate_schema,
        &left_predicate_schema,
        params,
    )?;
    let reduced = match (*kind, rejects_left, rejects_right) {
        (crate::ast::JoinKind::Left, _, true) | (crate::ast::JoinKind::Right, true, _) => {
            crate::ast::JoinKind::Inner
        }
        (crate::ast::JoinKind::Full, true, true) => crate::ast::JoinKind::Inner,
        (crate::ast::JoinKind::Full, true, false) => crate::ast::JoinKind::Left,
        (crate::ast::JoinKind::Full, false, true) => crate::ast::JoinKind::Right,
        (kind, _, _) => kind,
    };
    let mut changed = reduced != *kind;
    *kind = reduced;
    changed |= reduce_null_rejected_outer_joins(routines, left, predicate, params, context, outer)?;
    changed |=
        reduce_null_rejected_outer_joins(routines, right, predicate, params, context, right_outer)?;
    Ok(changed)
}

fn overlay_outer_schema(current: &RowSchema, outer: Option<&RowSchema>) -> RowSchema {
    let Some(outer) = outer else {
        return current.clone();
    };
    let columns = outer
        .identities()
        .iter()
        .enumerate()
        .map(|(position, identity)| (identity.clone(), outer.column_type(position).cloned()))
        .collect::<Vec<_>>();
    RowSchema::with_typed_outer_identities(current, &columns)
}

const TRUTH_FALSE: u8 = 1;
const TRUTH_TRUE: u8 = 2;
const TRUTH_NULL: u8 = 4;
const TRUTH_ANY: u8 = TRUTH_FALSE | TRUTH_TRUE | TRUTH_NULL;

fn predicate_rejects_null_extended_side(
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    side: &RowSchema,
    other: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    Ok(truth_values_with_null_side(routines, expression, side, other, params)? & TRUTH_TRUE == 0)
}

fn truth_values_with_null_side(
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    side: &RowSchema,
    other: &RowSchema,
    params: &[SQLParam],
) -> Result<u8, SQLError> {
    match expression {
        ScalarExpr::Literal(Value::Bool(value)) => {
            if *value {
                Ok(TRUTH_TRUE)
            } else {
                Ok(TRUTH_FALSE)
            }
        }
        ScalarExpr::Literal(Value::Null) => Ok(TRUTH_NULL),
        ScalarExpr::IsNull { expr, negated }
            if expression_is_null_with_side(routines, expr, side, other, params)? =>
        {
            if *negated {
                Ok(TRUTH_FALSE)
            } else {
                Ok(TRUTH_TRUE)
            }
        }
        ScalarExpr::Between { expr, low, high } => {
            let value_is_null = expression_is_null_with_side(routines, expr, side, other, params)?;
            let low_is_null = expression_is_null_with_side(routines, low, side, other, params)?;
            let high_is_null = expression_is_null_with_side(routines, high, side, other, params)?;
            if value_is_null || (low_is_null && high_is_null) {
                Ok(TRUTH_NULL)
            } else if low_is_null || high_is_null {
                Ok(TRUTH_FALSE | TRUTH_NULL)
            } else {
                Ok(TRUTH_ANY)
            }
        }
        expression if expression_is_null_with_side(routines, expression, side, other, params)? => {
            Ok(TRUTH_NULL)
        }
        ScalarExpr::Not(inner) => Ok(negate_truth_values(truth_values_with_null_side(
            routines, inner, side, other, params,
        )?)),
        ScalarExpr::And(items) => items.iter().try_fold(TRUTH_TRUE, |left, right| {
            Ok(combine_truth_values(
                left,
                truth_values_with_null_side(routines, right, side, other, params)?,
                true,
            ))
        }),
        ScalarExpr::Or(items) => items.iter().try_fold(TRUTH_FALSE, |left, right| {
            Ok(combine_truth_values(
                left,
                truth_values_with_null_side(routines, right, side, other, params)?,
                false,
            ))
        }),
        _ => Ok(TRUTH_ANY),
    }
}

fn expression_is_null_with_side(
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    side: &RowSchema,
    other: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    match expression {
        ScalarExpr::Literal(Value::Null) => Ok(true),
        ScalarExpr::Column(column) => Ok(side.unqualified_position(column).is_some()
            && other.unqualified_position(column).is_none()),
        ScalarExpr::QualifiedColumn { qualifier, column } => Ok(side
            .has_qualified_column(qualifier, column)
            && !other.has_qualified_column(qualifier, column)),
        ScalarExpr::Binary { lhs, rhs, .. } => Ok(expression_is_null_with_side(
            routines, lhs, side, other, params,
        )? || expression_is_null_with_side(
            routines, rhs, side, other, params,
        )?),
        ScalarExpr::UnaryMinus(inner) | ScalarExpr::Cast { expr: inner, .. } => {
            expression_is_null_with_side(routines, inner, side, other, params)
        }
        ScalarExpr::Between { expr, low, high } => {
            Ok(
                expression_is_null_with_side(routines, expr, side, other, params)?
                    || (expression_is_null_with_side(routines, low, side, other, params)?
                        && expression_is_null_with_side(routines, high, side, other, params)?),
            )
        }
        ScalarExpr::InList { expr, .. } => {
            expression_is_null_with_side(routines, expr, side, other, params)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            if !scalar_function_is_strict(
                routines,
                name,
                binding.as_ref(),
                args,
                side,
                other,
                params,
            )? {
                return Ok(false);
            }
            for argument in crate::scalar_call_arguments(args)? {
                if expression_is_null_with_side(routines, argument.value, side, other, params)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn scalar_function_is_strict(
    routines: &dyn RoutineResolution,
    name: &str,
    binding: Option<&crate::ast::FunctionBinding>,
    args: &[ScalarExpr],
    side: &RowSchema,
    other: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    let runtime_name = name
        .strip_prefix("pg_catalog.")
        .unwrap_or(name)
        .to_ascii_lowercase();
    if routines.has_registered_scalar_function(&runtime_name) {
        return Ok(false);
    }
    if let Some(strict) = crate::expr::bound_scalar_function_strictness(name, binding, args.len()) {
        return Ok(strict);
    }
    let schema = RowSchema::join(side, other, std::iter::empty::<String>());
    let (argument_names, argument_types, explicit_variadic) =
        crate::type_resolution::function_call_argument_signature(
            args,
            &schema,
            params,
            Some(routines),
        )?;
    if binding.is_none() && routines.lookup_visible_sql_functions(name)?.is_none() {
        return Ok(false);
    }
    Ok(routines
        .resolve_static_sql_function(
            name,
            binding,
            &argument_names,
            &argument_types,
            explicit_variadic,
        )?
        .is_some_and(|function| function.def.strict))
}

fn negate_truth_values(values: u8) -> u8 {
    (u8::from(values & TRUTH_FALSE != 0) * TRUTH_TRUE)
        | (u8::from(values & TRUTH_TRUE != 0) * TRUTH_FALSE)
        | (values & TRUTH_NULL)
}

fn combine_truth_values(left: u8, right: u8, and: bool) -> u8 {
    let mut output = 0;
    for lhs in [TRUTH_FALSE, TRUTH_TRUE, TRUTH_NULL] {
        if left & lhs == 0 {
            continue;
        }
        for rhs in [TRUTH_FALSE, TRUTH_TRUE, TRUTH_NULL] {
            if right & rhs == 0 {
                continue;
            }
            output |= if and {
                match (lhs, rhs) {
                    (TRUTH_FALSE, _) | (_, TRUTH_FALSE) => TRUTH_FALSE,
                    (TRUTH_TRUE, TRUTH_TRUE) => TRUTH_TRUE,
                    _ => TRUTH_NULL,
                }
            } else {
                match (lhs, rhs) {
                    (TRUTH_TRUE, _) | (_, TRUTH_TRUE) => TRUTH_TRUE,
                    (TRUTH_FALSE, TRUTH_FALSE) => TRUTH_FALSE,
                    _ => TRUTH_NULL,
                }
            };
        }
    }
    output
}
