//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_source_plan_schema, CteScope, Engine, RowSchema, SQLError, SQLParam, ScalarExpr,
    SourcePlan, Value,
};
use crate::sql::from_rows::{join_alias_input_schemas, resolve_join_using};

pub(super) fn reduce_null_rejected_outer_joins_to_fixpoint(
    engine: &Engine,
    source: &mut SourcePlan,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    loop {
        let mut qualifications = predicate.iter().map(|expr| (*expr).clone()).collect();
        collect_inner_join_qualifications(source, &mut qualifications);
        let mut changed = false;
        for qualification in &qualifications {
            changed |= reduce_null_rejected_outer_joins(
                engine,
                source,
                qualification,
                params,
                ctes,
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
        uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross
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
    engine: &Engine,
    source: &mut SourcePlan,
    predicate: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope,
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
    let left_schema = bind_source_plan_schema(engine, left, params, ctes, outer)?;
    let implicit_lateral_function = matches!(
        right.as_ref(),
        SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
    );
    let right_scope =
        (*lateral || implicit_lateral_function).then(|| overlay_outer_schema(&left_schema, outer));
    let right_outer = right_scope.as_ref().or(outer);
    let right_schema = bind_source_plan_schema(engine, right, params, ctes, right_outer)?;
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
        engine,
        predicate,
        &left_predicate_schema,
        &right_predicate_schema,
        params,
    )?;
    let rejects_right = predicate_rejects_null_extended_side(
        engine,
        predicate,
        &right_predicate_schema,
        &left_predicate_schema,
        params,
    )?;
    let reduced = match (*kind, rejects_left, rejects_right) {
        (uqa_sql::ast::JoinKind::Left, _, true) | (uqa_sql::ast::JoinKind::Right, true, _) => {
            uqa_sql::ast::JoinKind::Inner
        }
        (uqa_sql::ast::JoinKind::Full, true, true) => uqa_sql::ast::JoinKind::Inner,
        (uqa_sql::ast::JoinKind::Full, true, false) => uqa_sql::ast::JoinKind::Left,
        (uqa_sql::ast::JoinKind::Full, false, true) => uqa_sql::ast::JoinKind::Right,
        (kind, _, _) => kind,
    };
    let mut changed = reduced != *kind;
    *kind = reduced;
    changed |= reduce_null_rejected_outer_joins(engine, left, predicate, params, ctes, outer)?;
    changed |=
        reduce_null_rejected_outer_joins(engine, right, predicate, params, ctes, right_outer)?;
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
    engine: &Engine,
    expression: &ScalarExpr,
    side: &RowSchema,
    other: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    Ok(truth_values_with_null_side(engine, expression, side, other, params)? & TRUTH_TRUE == 0)
}

fn truth_values_with_null_side(
    engine: &Engine,
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
            if expression_is_null_with_side(engine, expr, side, other, params)? =>
        {
            if *negated {
                Ok(TRUTH_FALSE)
            } else {
                Ok(TRUTH_TRUE)
            }
        }
        ScalarExpr::Between { expr, low, high } => {
            let value_is_null = expression_is_null_with_side(engine, expr, side, other, params)?;
            let low_is_null = expression_is_null_with_side(engine, low, side, other, params)?;
            let high_is_null = expression_is_null_with_side(engine, high, side, other, params)?;
            if value_is_null || (low_is_null && high_is_null) {
                Ok(TRUTH_NULL)
            } else if low_is_null || high_is_null {
                Ok(TRUTH_FALSE | TRUTH_NULL)
            } else {
                Ok(TRUTH_ANY)
            }
        }
        expression if expression_is_null_with_side(engine, expression, side, other, params)? => {
            Ok(TRUTH_NULL)
        }
        ScalarExpr::Not(inner) => Ok(negate_truth_values(truth_values_with_null_side(
            engine, inner, side, other, params,
        )?)),
        ScalarExpr::And(items) => items.iter().try_fold(TRUTH_TRUE, |left, right| {
            Ok(combine_truth_values(
                left,
                truth_values_with_null_side(engine, right, side, other, params)?,
                true,
            ))
        }),
        ScalarExpr::Or(items) => items.iter().try_fold(TRUTH_FALSE, |left, right| {
            Ok(combine_truth_values(
                left,
                truth_values_with_null_side(engine, right, side, other, params)?,
                false,
            ))
        }),
        _ => Ok(TRUTH_ANY),
    }
}

fn expression_is_null_with_side(
    engine: &Engine,
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
            engine, lhs, side, other, params,
        )? || expression_is_null_with_side(
            engine, rhs, side, other, params,
        )?),
        ScalarExpr::UnaryMinus(inner) | ScalarExpr::Cast { expr: inner, .. } => {
            expression_is_null_with_side(engine, inner, side, other, params)
        }
        ScalarExpr::Between { expr, low, high } => {
            Ok(
                expression_is_null_with_side(engine, expr, side, other, params)?
                    || (expression_is_null_with_side(engine, low, side, other, params)?
                        && expression_is_null_with_side(engine, high, side, other, params)?),
            )
        }
        ScalarExpr::InList { expr, .. } => {
            expression_is_null_with_side(engine, expr, side, other, params)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            if !scalar_function_is_strict(
                engine,
                name,
                binding.as_ref(),
                args,
                side,
                other,
                params,
            )? {
                return Ok(false);
            }
            for argument in args.iter().map(row_lock_function_argument_value) {
                if expression_is_null_with_side(engine, argument, side, other, params)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn scalar_function_is_strict(
    engine: &Engine,
    name: &str,
    binding: Option<&uqa_sql::ast::FunctionBinding>,
    args: &[ScalarExpr],
    side: &RowSchema,
    other: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    let runtime_name = name
        .strip_prefix("pg_catalog.")
        .unwrap_or(name)
        .to_ascii_lowercase();
    if engine.has_registered_scalar_function(&runtime_name) {
        return Ok(false);
    }
    if let Some(strict) = uqa_sql::expr::builtin_scalar_function_strictness(name, args.len()) {
        return Ok(strict);
    }
    if engine.lookup_sql_functions(name).is_none() {
        return Ok(false);
    }
    let schema = RowSchema::join(side, other, std::iter::empty::<String>());
    let mut argument_names = Vec::with_capacity(args.len());
    let mut argument_types = Vec::with_capacity(args.len());
    for argument in args {
        let (argument_name, value) = row_lock_function_argument(argument);
        argument_names.push(argument_name);
        argument_types.push(uqa_execution::common_context_expression_type(
            value,
            &schema,
            params,
            Some(engine),
        )?);
    }
    Ok(engine
        .resolve_static_sql_function(name, binding, &argument_names, &argument_types)?
        .is_some_and(|function| function.def.strict))
}

fn row_lock_function_argument(expression: &ScalarExpr) -> (Option<String>, &ScalarExpr) {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return (None, expression);
    };
    if name != uqa_sql::expr::NAMED_ARG_FUNCTION {
        return (None, expression);
    }
    let argument_name = args.first().and_then(|name| match name {
        ScalarExpr::Literal(Value::Str(name)) => Some(name.clone()),
        _ => None,
    });
    (argument_name, args.get(1).unwrap_or(expression))
}

fn row_lock_function_argument_value(expression: &ScalarExpr) -> &ScalarExpr {
    row_lock_function_argument(expression).1
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
