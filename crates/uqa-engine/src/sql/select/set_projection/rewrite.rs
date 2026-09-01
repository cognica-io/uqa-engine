//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-returning and aggregate dependency expression rewrites.

use uqa_core::Value;
use uqa_execution::{FunctionTypeResolver, RowSchema, ScalarExpr, ScalarFrameBound};
use uqa_planner::{ProjectionPlan, QueryBlockPlan};
use uqa_sql::{SQLError, SQLParam};

use super::super::projection_columns;
use super::validation::{
    expression_may_return_set, function_may_return_set, resolve_set_function_binding,
};
use super::{
    AggregateOutputProjectionPlan, Engine, GroupSetProjectionPlan, ProjectionTarget,
    SetFunctionCall,
};
use crate::sql::aggregates::{exprs_match, is_aggregate};

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub(super) fn rewrite_set_calls(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
    mut expression: ScalarExpr,
    calls: &mut Vec<SetFunctionCall>,
    call_relation: uqa_sql::ast::InternalRelationId,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<ScalarExpr, SQLError> {
    let descendant_start = calls.len();
    match &mut expression {
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                *argument = rewrite_set_calls(
                    engine,
                    resolver,
                    argument.clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
            for order in order_by {
                order.expr = rewrite_set_calls(
                    engine,
                    resolver,
                    order.expr.clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
            if let Some(filter) = filter {
                **filter = rewrite_set_calls(
                    engine,
                    resolver,
                    (**filter).clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            for item in items {
                *item = rewrite_set_calls(
                    engine,
                    resolver,
                    item.clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            **lhs = rewrite_set_calls(
                engine,
                resolver,
                (**lhs).clone(),
                calls,
                call_relation,
                schema,
                params,
            )?;
            **rhs = rewrite_set_calls(
                engine,
                resolver,
                (**rhs).clone(),
                calls,
                call_relation,
                schema,
                params,
            )?;
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            **inner = rewrite_set_calls(
                engine,
                resolver,
                (**inner).clone(),
                calls,
                call_relation,
                schema,
                params,
            )?;
        }
        ScalarExpr::Between { expr, low, high } => {
            **expr = rewrite_set_calls(
                engine,
                resolver,
                (**expr).clone(),
                calls,
                call_relation,
                schema,
                params,
            )?;
            **low = rewrite_set_calls(
                engine,
                resolver,
                (**low).clone(),
                calls,
                call_relation,
                schema,
                params,
            )?;
            **high = rewrite_set_calls(
                engine,
                resolver,
                (**high).clone(),
                calls,
                call_relation,
                schema,
                params,
            )?;
        }
        ScalarExpr::InList { expr, list, .. } => {
            **expr = rewrite_set_calls(
                engine,
                resolver,
                (**expr).clone(),
                calls,
                call_relation,
                schema,
                params,
            )?;
            for item in list {
                *item = rewrite_set_calls(
                    engine,
                    resolver,
                    item.clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for argument in args {
                *argument = rewrite_set_calls(
                    engine,
                    resolver,
                    argument.clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
            for item in &mut spec.partition_by {
                *item = rewrite_set_calls(
                    engine,
                    resolver,
                    item.clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
            for order in &mut spec.order_by {
                order.expr = rewrite_set_calls(
                    engine,
                    resolver,
                    order.expr.clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
            if let Some(frame) = &mut spec.frame {
                rewrite_set_frame_bound(
                    engine,
                    resolver,
                    &mut frame.start,
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
                rewrite_set_frame_bound(
                    engine,
                    resolver,
                    &mut frame.end,
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                **base = rewrite_set_calls(
                    engine,
                    resolver,
                    (**base).clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
            for (condition, result) in when {
                *condition = rewrite_set_calls(
                    engine,
                    resolver,
                    condition.clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
                *result = rewrite_set_calls(
                    engine,
                    resolver,
                    result.clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
            if let Some(branch) = else_branch {
                **branch = rewrite_set_calls(
                    engine,
                    resolver,
                    (**branch).clone(),
                    calls,
                    call_relation,
                    schema,
                    params,
                )?;
            }
        }
        ScalarExpr::InSubquery { expr, .. } => {
            **expr = rewrite_set_calls(
                engine,
                resolver,
                (**expr).clone(),
                calls,
                call_relation,
                schema,
                params,
            )?;
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
    if let ScalarExpr::Func {
        name,
        binding,
        args,
        ..
    } = &expression
    {
        if function_may_return_set(
            engine,
            resolver,
            name,
            binding.as_ref(),
            args,
            schema,
            params,
        )? {
            let binding = resolve_set_function_binding(
                engine,
                resolver,
                name,
                binding.as_ref(),
                args,
                schema,
                params,
            )?
            .or_else(|| binding.clone());
            let level = calls[descendant_start..]
                .iter()
                .map(|call| call.level + 1)
                .max()
                .unwrap_or(0);
            let placeholder = call_relation.column(calls.len());
            calls.push(SetFunctionCall {
                placeholder,
                name: name.clone(),
                binding,
                args: args.clone(),
                level,
            });
            return Ok(ScalarExpr::InternalColumn(placeholder));
        }
    }
    Ok(expression)
}

fn rewrite_set_frame_bound(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
    bound: &mut ScalarFrameBound,
    calls: &mut Vec<SetFunctionCall>,
    call_relation: uqa_sql::ast::InternalRelationId,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            **expression = rewrite_set_calls(
                engine,
                resolver,
                (**expression).clone(),
                calls,
                call_relation,
                schema,
                params,
            )?;
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
    Ok(())
}

fn replace_group_set_expression(
    expression: &mut ScalarExpr,
    mappings: &[(ScalarExpr, uqa_sql::ast::InternalColumnRef)],
) {
    if let Some((_, column)) = mappings
        .iter()
        .find(|(group, _)| exprs_match(expression, group))
    {
        *expression = ScalarExpr::InternalColumn(*column);
        return;
    }
    match expression {
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                replace_group_set_expression(argument, mappings);
            }
            for order in order_by {
                replace_group_set_expression(&mut order.expr, mappings);
            }
            if let Some(filter) = filter {
                replace_group_set_expression(filter, mappings);
            }
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            for item in items {
                replace_group_set_expression(item, mappings);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            replace_group_set_expression(lhs, mappings);
            replace_group_set_expression(rhs, mappings);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            replace_group_set_expression(inner, mappings);
        }
        ScalarExpr::Between { expr, low, high } => {
            replace_group_set_expression(expr, mappings);
            replace_group_set_expression(low, mappings);
            replace_group_set_expression(high, mappings);
        }
        ScalarExpr::InList { expr, list, .. } => {
            replace_group_set_expression(expr, mappings);
            for item in list {
                replace_group_set_expression(item, mappings);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for argument in args {
                replace_group_set_expression(argument, mappings);
            }
            for item in &mut spec.partition_by {
                replace_group_set_expression(item, mappings);
            }
            for order in &mut spec.order_by {
                replace_group_set_expression(&mut order.expr, mappings);
            }
            if let Some(frame) = &mut spec.frame {
                replace_group_set_frame_bound(&mut frame.start, mappings);
                replace_group_set_frame_bound(&mut frame.end, mappings);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                replace_group_set_expression(base, mappings);
            }
            for (condition, result) in when {
                replace_group_set_expression(condition, mappings);
                replace_group_set_expression(result, mappings);
            }
            if let Some(branch) = else_branch {
                replace_group_set_expression(branch, mappings);
            }
        }
        ScalarExpr::InSubquery { expr, .. } => {
            replace_group_set_expression(expr, mappings);
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
}

fn replace_group_set_frame_bound(
    bound: &mut ScalarFrameBound,
    mappings: &[(ScalarExpr, uqa_sql::ast::InternalColumnRef)],
) {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            replace_group_set_expression(expression, mappings);
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
}

pub(in crate::sql) fn prepare_group_set_projection(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<GroupSetProjectionPlan>, SQLError> {
    let mut groups = Vec::new();
    for expression in statement
        .group_by
        .iter()
        .chain(statement.grouping_sets.iter().flatten())
    {
        if expression_may_return_set(engine, resolver, expression, schema, params)?
            && !groups
                .iter()
                .any(|existing| exprs_match(existing, expression))
        {
            groups.push(expression.clone());
        }
    }
    if groups.is_empty() {
        return Ok(None);
    }

    let relation = uqa_sql::ast::InternalRelationId::allocate();
    let mappings = groups
        .iter()
        .enumerate()
        .map(|(index, expression)| (expression.clone(), relation.column(index)))
        .collect::<Vec<_>>();
    let projections = mappings
        .iter()
        .map(|(expression, column)| (ProjectionTarget::Internal(*column), expression.clone()))
        .collect();
    let mut rewritten = statement.clone();
    let projection_labels = projection_columns(&rewritten.projections);
    for expression in &mut rewritten.group_by {
        replace_group_set_expression(expression, &mappings);
    }
    for set in &mut rewritten.grouping_sets {
        for expression in set {
            replace_group_set_expression(expression, &mappings);
        }
    }
    for (projection, label) in rewritten.projections.iter_mut().zip(projection_labels) {
        if projection.alias.is_none() {
            projection.alias = Some(label);
        }
        replace_group_set_expression(&mut projection.expr, &mappings);
    }
    if let Some(having) = &mut rewritten.having {
        replace_group_set_expression(having, &mappings);
    }
    for order in &mut rewritten.order_by {
        replace_group_set_expression(&mut order.expr, &mappings);
    }
    for expression in &mut rewritten.distinct_on {
        replace_group_set_expression(expression, &mappings);
    }
    Ok(Some(GroupSetProjectionPlan {
        statement: rewritten,
        projections,
    }))
}

fn capture_aggregate_dependency(
    expression: &ScalarExpr,
    dependencies: &mut Vec<ProjectionPlan>,
) -> ScalarExpr {
    let position = dependencies.len();
    dependencies.push(ProjectionPlan {
        expr: expression.clone(),
        alias: None,
    });
    ScalarExpr::Position(position)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
fn rewrite_aggregate_dependencies(
    engine: &Engine,
    group_by: &[ScalarExpr],
    expression: &ScalarExpr,
    dependencies: &mut Vec<ProjectionPlan>,
) -> ScalarExpr {
    if is_aggregate(engine, expression)
        || group_by.iter().any(|group| exprs_match(expression, group))
    {
        return capture_aggregate_dependency(expression, dependencies);
    }
    match expression {
        ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. } => {
            capture_aggregate_dependency(expression, dependencies)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
            name: name.clone(),
            binding: binding.clone(),
            args: args
                .iter()
                .map(|argument| {
                    rewrite_aggregate_dependencies(engine, group_by, argument, dependencies)
                })
                .collect(),
            distinct: *distinct,
            order_by: order_by
                .iter()
                .map(|order| {
                    let mut order = order.clone();
                    order.expr =
                        rewrite_aggregate_dependencies(engine, group_by, &order.expr, dependencies);
                    order
                })
                .collect(),
            filter: filter.as_deref().map(|filter| {
                Box::new(rewrite_aggregate_dependencies(
                    engine,
                    group_by,
                    filter,
                    dependencies,
                ))
            }),
        },
        ScalarExpr::Array(items) => ScalarExpr::Array(
            items
                .iter()
                .map(|item| rewrite_aggregate_dependencies(engine, group_by, item, dependencies))
                .collect(),
        ),
        ScalarExpr::Row(items) => ScalarExpr::Row(
            items
                .iter()
                .map(|item| rewrite_aggregate_dependencies(engine, group_by, item, dependencies))
                .collect(),
        ),
        ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(rewrite_aggregate_dependencies(
                engine,
                group_by,
                lhs,
                dependencies,
            )),
            rhs: Box::new(rewrite_aggregate_dependencies(
                engine,
                group_by,
                rhs,
                dependencies,
            )),
        },
        ScalarExpr::Not(inner) => ScalarExpr::Not(Box::new(rewrite_aggregate_dependencies(
            engine,
            group_by,
            inner,
            dependencies,
        ))),
        ScalarExpr::UnaryMinus(inner) => ScalarExpr::UnaryMinus(Box::new(
            rewrite_aggregate_dependencies(engine, group_by, inner, dependencies),
        )),
        ScalarExpr::And(items) => ScalarExpr::And(
            items
                .iter()
                .map(|item| rewrite_aggregate_dependencies(engine, group_by, item, dependencies))
                .collect(),
        ),
        ScalarExpr::Or(items) => ScalarExpr::Or(
            items
                .iter()
                .map(|item| rewrite_aggregate_dependencies(engine, group_by, item, dependencies))
                .collect(),
        ),
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(rewrite_aggregate_dependencies(
                engine,
                group_by,
                expr,
                dependencies,
            )),
            negated: *negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(rewrite_aggregate_dependencies(
                engine,
                group_by,
                expr,
                dependencies,
            )),
            low: Box::new(rewrite_aggregate_dependencies(
                engine,
                group_by,
                low,
                dependencies,
            )),
            high: Box::new(rewrite_aggregate_dependencies(
                engine,
                group_by,
                high,
                dependencies,
            )),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(rewrite_aggregate_dependencies(
                engine,
                group_by,
                expr,
                dependencies,
            )),
            list: list
                .iter()
                .map(|item| rewrite_aggregate_dependencies(engine, group_by, item, dependencies))
                .collect(),
            negated: *negated,
        },
        ScalarExpr::WindowCall { name, args, spec } => {
            let mut spec = spec.clone();
            spec.partition_by = spec
                .partition_by
                .iter()
                .map(|item| rewrite_aggregate_dependencies(engine, group_by, item, dependencies))
                .collect();
            for order in &mut spec.order_by {
                order.expr =
                    rewrite_aggregate_dependencies(engine, group_by, &order.expr, dependencies);
            }
            if let Some(frame) = &mut spec.frame {
                rewrite_aggregate_frame_bound(engine, group_by, &mut frame.start, dependencies);
                rewrite_aggregate_frame_bound(engine, group_by, &mut frame.end, dependencies);
            }
            ScalarExpr::WindowCall {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|argument| {
                        rewrite_aggregate_dependencies(engine, group_by, argument, dependencies)
                    })
                    .collect(),
                spec,
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: base.as_deref().map(|base| {
                Box::new(rewrite_aggregate_dependencies(
                    engine,
                    group_by,
                    base,
                    dependencies,
                ))
            }),
            when: when
                .iter()
                .map(|(condition, result)| {
                    (
                        rewrite_aggregate_dependencies(engine, group_by, condition, dependencies),
                        rewrite_aggregate_dependencies(engine, group_by, result, dependencies),
                    )
                })
                .collect(),
            else_branch: else_branch.as_deref().map(|branch| {
                Box::new(rewrite_aggregate_dependencies(
                    engine,
                    group_by,
                    branch,
                    dependencies,
                ))
            }),
        },
        ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(rewrite_aggregate_dependencies(
                engine,
                group_by,
                expr,
                dependencies,
            )),
            ty: ty.clone(),
        },
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(rewrite_aggregate_dependencies(
                engine,
                group_by,
                expr,
                dependencies,
            )),
            subquery: *subquery,
            negated: *negated,
        },
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => expression.clone(),
    }
}

fn rewrite_aggregate_frame_bound(
    engine: &Engine,
    group_by: &[ScalarExpr],
    bound: &mut ScalarFrameBound,
    dependencies: &mut Vec<ProjectionPlan>,
) {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            **expression =
                rewrite_aggregate_dependencies(engine, group_by, expression, dependencies);
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
}

pub(in crate::sql) fn prepare_aggregate_output_projection(
    engine: &Engine,
    statement: &QueryBlockPlan,
    internal_targets: &[(usize, uqa_sql::ast::InternalColumnRef)],
) -> AggregateOutputProjectionPlan {
    let labels = projection_columns(&statement.projections);
    let mut dependencies = Vec::new();
    let projections = statement
        .projections
        .iter()
        .enumerate()
        .zip(labels)
        .map(|((position, projection), label)| {
            let target = internal_targets
                .iter()
                .find(|(target_position, _)| *target_position == position)
                .map_or_else(
                    || ProjectionTarget::Column(label),
                    |(_, column)| ProjectionTarget::Internal(*column),
                );
            (
                target,
                rewrite_aggregate_dependencies(
                    engine,
                    &statement.group_by,
                    &projection.expr,
                    &mut dependencies,
                ),
            )
        })
        .collect();
    if dependencies.is_empty() {
        dependencies.push(ProjectionPlan {
            expr: ScalarExpr::Literal(Value::Int(1)),
            alias: None,
        });
    }
    let mut aggregate_statement = statement.clone();
    aggregate_statement.projections = dependencies;
    AggregateOutputProjectionPlan {
        statement: aggregate_statement,
        projections,
    }
}
