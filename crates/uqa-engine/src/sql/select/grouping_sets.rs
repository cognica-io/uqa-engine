//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible grouping-set preparation after input schema binding.

use std::collections::HashSet;

use uqa_core::Value;
use uqa_execution::{RowSchema, ScalarExpr, ScalarFrameBound};
use uqa_sql::ast::ColumnType;

use super::{Engine, QueryBlockPlan, SQLError, SQLParam};

/// `GROUP BY DISTINCT` operates on expanded grouping sets after parse analysis. At this point the input schema is known, so equivalent qualified and unqualified columns can share one identity and exact no-op casts can be removed without conflating expressions that `PostgreSQL` resolves to different operator inputs.
pub(in crate::sql) fn prepare_distinct_grouping_sets(
    engine: &Engine,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<QueryBlockPlan>, SQLError> {
    if !statement.group_distinct {
        return Ok(None);
    }

    let mut prepared = statement.clone();
    prepared.group_distinct = false;
    let mut seen = HashSet::with_capacity(prepared.grouping_sets.len());
    let mut distinct = Vec::with_capacity(prepared.grouping_sets.len());
    for grouping_set in std::mem::take(&mut prepared.grouping_sets) {
        let identity = grouping_set_identity(engine, &grouping_set, schema, params)?;
        if seen.insert(identity) {
            distinct.push(grouping_set);
        }
    }
    prepared.grouping_sets = distinct;
    Ok(Some(prepared))
}

fn grouping_set_identity(
    engine: &Engine,
    grouping_set: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Vec<Vec<u8>>, SQLError> {
    let mut identity = grouping_set
        .iter()
        .map(|expression| expression_identity(engine, expression, schema, params))
        .collect::<Result<Vec<_>, _>>()?;
    identity.sort_unstable();
    identity.dedup();
    Ok(identity)
}

fn expression_identity(
    engine: &Engine,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Vec<u8>, SQLError> {
    let expression = uqa_execution::bind_type_introspection_with_resolver(
        expression.clone(),
        schema,
        params,
        engine,
    );
    let expression = normalize_expression(engine, expression, schema, params)?;
    serde_json::to_vec(&expression).map_err(|error| {
        SQLError::Internal(format!(
            "serialize GROUP BY DISTINCT expression identity: {error}"
        ))
    })
}

fn normalize_expression(
    engine: &Engine,
    expression: ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<ScalarExpr, SQLError> {
    Ok(match expression {
        ScalarExpr::Column(column) => schema
            .unqualified_position(&column)
            .map_or(ScalarExpr::Column(column), ScalarExpr::Position),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            schema.qualified_position(&qualifier, &column).map_or(
                ScalarExpr::QualifiedColumn { qualifier, column },
                ScalarExpr::Position,
            )
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => {
            let name = canonical_function_name(name);
            let targets = function_argument_targets(engine, &name, &args, schema, params)?;
            ScalarExpr::Func {
                name,
                binding,
                args: args
                    .into_iter()
                    .zip(targets)
                    .map(|(argument, target)| {
                        normalize_unknown_literal(engine, argument, target.as_ref(), schema, params)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                distinct,
                order_by: order_by
                    .into_iter()
                    .map(|mut order| {
                        order.expr = normalize_expression(engine, order.expr, schema, params)?;
                        Ok(order)
                    })
                    .collect::<Result<Vec<_>, SQLError>>()?,
                filter: filter
                    .map(|expression| {
                        normalize_expression(engine, *expression, schema, params).map(Box::new)
                    })
                    .transpose()?,
            }
        }
        ScalarExpr::Array(items) => {
            ScalarExpr::Array(normalize_items(engine, items, schema, params)?)
        }
        ScalarExpr::Row(items) => ScalarExpr::Row(normalize_items(engine, items, schema, params)?),
        ScalarExpr::Binary { op, lhs, rhs } => {
            let left_type = expression_type(engine, &lhs, schema, params)?;
            let right_type = expression_type(engine, &rhs, schema, params)?;
            ScalarExpr::Binary {
                op,
                lhs: Box::new(normalize_unknown_literal(
                    engine,
                    *lhs,
                    left_type.is_none().then_some(right_type.as_ref()).flatten(),
                    schema,
                    params,
                )?),
                rhs: Box::new(normalize_unknown_literal(
                    engine,
                    *rhs,
                    right_type.is_none().then_some(left_type.as_ref()).flatten(),
                    schema,
                    params,
                )?),
            }
        }
        ScalarExpr::UnaryMinus(expression) => ScalarExpr::UnaryMinus(Box::new(
            normalize_expression(engine, *expression, schema, params)?,
        )),
        ScalarExpr::Not(expression) => ScalarExpr::Not(Box::new(normalize_expression(
            engine,
            *expression,
            schema,
            params,
        )?)),
        ScalarExpr::And(items) => ScalarExpr::And(normalize_items(engine, items, schema, params)?),
        ScalarExpr::Or(items) => ScalarExpr::Or(normalize_items(engine, items, schema, params)?),
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(normalize_expression(engine, *expr, schema, params)?),
            negated,
        },
        ScalarExpr::Between { expr, low, high } => {
            let target = expression_type(engine, &expr, schema, params)?;
            ScalarExpr::Between {
                expr: Box::new(normalize_expression(engine, *expr, schema, params)?),
                low: Box::new(normalize_unknown_literal(
                    engine,
                    *low,
                    target.as_ref(),
                    schema,
                    params,
                )?),
                high: Box::new(normalize_unknown_literal(
                    engine,
                    *high,
                    target.as_ref(),
                    schema,
                    params,
                )?),
            }
        }
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => {
            let target = expression_type(engine, &expr, schema, params)?;
            ScalarExpr::InList {
                expr: Box::new(normalize_expression(engine, *expr, schema, params)?),
                list: list
                    .into_iter()
                    .map(|item| {
                        normalize_unknown_literal(engine, item, target.as_ref(), schema, params)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                negated,
            }
        }
        ScalarExpr::WindowCall {
            name,
            args,
            mut spec,
        } => {
            spec.partition_by = normalize_items(engine, spec.partition_by, schema, params)?;
            for order in &mut spec.order_by {
                order.expr = normalize_expression(engine, order.expr.clone(), schema, params)?;
            }
            if let Some(frame) = &mut spec.frame {
                normalize_frame_bound(engine, &mut frame.start, schema, params)?;
                normalize_frame_bound(engine, &mut frame.end, schema, params)?;
            }
            ScalarExpr::WindowCall {
                name: canonical_function_name(name),
                args: normalize_items(engine, args, schema, params)?,
                spec,
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: base
                .map(|expression| {
                    normalize_expression(engine, *expression, schema, params).map(Box::new)
                })
                .transpose()?,
            when: when
                .into_iter()
                .map(|(condition, result)| {
                    Ok((
                        normalize_expression(engine, condition, schema, params)?,
                        normalize_expression(engine, result, schema, params)?,
                    ))
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            else_branch: else_branch
                .map(|expression| {
                    normalize_expression(engine, *expression, schema, params).map(Box::new)
                })
                .transpose()?,
        },
        ScalarExpr::Cast { expr, ty } => {
            let source_type = expression_type(engine, &expr, schema, params)?;
            let target_type = ColumnType::from_sql_name(&ty)?;
            let expression = normalize_expression(engine, *expr, schema, params)?;
            if source_type.as_ref() == Some(&target_type) {
                expression
            } else {
                ScalarExpr::Cast {
                    expr: Box::new(expression),
                    ty: target_type.sql_name(),
                }
            }
        }
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(normalize_expression(engine, *expr, schema, params)?),
            subquery,
            negated,
        },
        expression @ (ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default
        | ScalarExpr::Position(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }) => expression,
    })
}

fn normalize_items(
    engine: &Engine,
    items: Vec<ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Vec<ScalarExpr>, SQLError> {
    items
        .into_iter()
        .map(|item| normalize_expression(engine, item, schema, params))
        .collect()
}

fn normalize_unknown_literal(
    engine: &Engine,
    expression: ScalarExpr,
    target: Option<&ColumnType>,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<ScalarExpr, SQLError> {
    if matches!(expression, ScalarExpr::Literal(Value::Null)) {
        if let Some(target) = target {
            return normalize_expression(
                engine,
                ScalarExpr::Cast {
                    expr: Box::new(expression),
                    ty: target.sql_name(),
                },
                schema,
                params,
            );
        }
    }
    normalize_expression(engine, expression, schema, params)
}

fn normalize_frame_bound(
    engine: &Engine,
    bound: &mut ScalarFrameBound,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            **expression = normalize_expression(engine, (**expression).clone(), schema, params)?;
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
    Ok(())
}

fn expression_type(
    engine: &Engine,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<ColumnType>, SQLError> {
    uqa_execution::scalar_type_with_resolver(expression, schema, params, engine)
}

fn function_argument_targets(
    engine: &Engine,
    name: &str,
    arguments: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let mut targets = vec![None; arguments.len()];
    match name {
        "upper" | "lower" | "casefold" | "initcap" => {
            if !targets.is_empty() {
                targets[0] = Some(ColumnType::Text);
            }
        }
        "concat_op" if arguments.len() == 2 => {
            let types = arguments
                .iter()
                .map(|argument| expression_type(engine, argument, schema, params))
                .collect::<Result<Vec<_>, _>>()?;
            for position in 0..2 {
                if types[position].is_none() {
                    targets[position] = Some(concat_argument_type(types[1 - position].as_ref()));
                }
            }
        }
        _ => {}
    }
    Ok(targets)
}

fn concat_argument_type(other: Option<&ColumnType>) -> ColumnType {
    match other {
        Some(array @ ColumnType::Array(_)) => array.clone(),
        Some(ColumnType::JsonB) => ColumnType::JsonB,
        _ => ColumnType::Text,
    }
}

fn canonical_function_name(name: String) -> String {
    let lower = name.to_ascii_lowercase();
    match lower.strip_prefix("pg_catalog.") {
        Some(unqualified) => unqualified.to_owned(),
        None => name,
    }
}
