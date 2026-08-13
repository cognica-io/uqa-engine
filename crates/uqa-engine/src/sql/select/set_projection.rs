//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-style set-returning SELECT-list projection.

use uqa_core::Value;
use uqa_execution::{
    eval_call_arguments, Batch, ExecResult, PhysicalOperator, Project, ProjectRows, RowSchema,
    ScalarEvalContext, ScalarExpr, ScalarFrameBound, SharedExpressionEvaluator,
};
use uqa_planner::{ProjectionPlan, QueryBlockPlan, SourcePlan};
use uqa_sql::{ResultRow, SQLError, SQLParam};

use super::{projection_columns, CteScope, Engine, PhysicalProjection, ScopedEngineHook};
use crate::sql::aggregates::{exprs_match, is_aggregate};
use crate::sql::scalar::PlanSubqueryArena;

const SET_VALUE_COLUMN_PREFIX: &str = "\0uqa.set_value.";

#[derive(Clone)]
struct SetFunctionCall {
    placeholder: String,
    name: String,
    args: Vec<ScalarExpr>,
    level: usize,
}

struct SetProjectionPlan {
    projections: Vec<PhysicalProjection>,
    calls: Vec<SetFunctionCall>,
    output_batch_size: usize,
}

pub(in crate::sql) struct AggregateSetProjectionPlan {
    pub(in crate::sql) statement: QueryBlockPlan,
    pub(in crate::sql) projections: Vec<PhysicalProjection>,
}

pub(in crate::sql) struct GroupSetProjectionPlan {
    pub(in crate::sql) statement: QueryBlockPlan,
    pub(in crate::sql) projections: Vec<PhysicalProjection>,
}

enum SetFunctionState {
    Scalar(Value),
    Set { rows: ProjectRows, exhausted: bool },
}

struct SetExpansion {
    input: ResultRow,
    calls: Vec<SetFunctionState>,
    has_set: bool,
    scalar_emitted: bool,
}

impl SetExpansion {
    fn next_values(&mut self) -> ExecResult<Option<Vec<Value>>> {
        if !self.has_set {
            if self.scalar_emitted {
                return Ok(None);
            }
            self.scalar_emitted = true;
            return Ok(Some(
                self.calls
                    .iter()
                    .map(|call| match call {
                        SetFunctionState::Scalar(value) => value.clone(),
                        SetFunctionState::Set { .. } => unreachable!("has_set is false"),
                    })
                    .collect(),
            ));
        }

        let mut produced = false;
        let mut values = Vec::with_capacity(self.calls.len());
        for call in &mut self.calls {
            match call {
                SetFunctionState::Scalar(value) => values.push(value.clone()),
                SetFunctionState::Set { rows, exhausted } => {
                    if *exhausted {
                        values.push(Value::Null);
                        continue;
                    }
                    if let Some(row) = rows.next() {
                        produced = true;
                        values.push(set_row_value(row?));
                    } else {
                        *exhausted = true;
                        values.push(Value::Null);
                    }
                }
            }
        }
        Ok(produced.then_some(values))
    }
}

fn set_row_value(row: ResultRow) -> Value {
    if row.len() == 1 {
        return row.into_values().next().unwrap_or(Value::Null);
    }
    Value::Map(row)
}

fn builtin_returns_set(name: &str) -> bool {
    matches!(
        name,
        "generate_series"
            | "unnest"
            | "regexp_split_to_table"
            | "string_to_table"
            | "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "json_each"
            | "jsonb_each"
            | "json_each_text"
            | "jsonb_each_text"
            | "json_object_keys"
            | "jsonb_object_keys"
    )
}

fn function_may_return_set(engine: &Engine, name: &str) -> bool {
    let identity = name.to_ascii_lowercase();
    let builtin = crate::sql::builtin_function_dispatch_name(&identity);
    builtin_returns_set(&builtin)
        || engine.has_registered_table_function(&identity)
        || engine
            .lookup_sql_functions(name)
            .is_some_and(|functions| functions.iter().any(|function| function.def.returns_set()))
}

pub(in crate::sql) fn projections_may_return_set(
    engine: &Engine,
    projections: &[PhysicalProjection],
) -> bool {
    projections
        .iter()
        .any(|(_, expression)| expression_may_return_set(engine, expression))
}

pub(in crate::sql) fn expression_may_return_set(engine: &Engine, expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Func {
            name,
            args,
            order_by,
            filter,
            ..
        } => {
            function_may_return_set(engine, name)
                || args
                    .iter()
                    .any(|argument| expression_may_return_set(engine, argument))
                || order_by
                    .iter()
                    .any(|order| expression_may_return_set(engine, &order.expr))
                || filter
                    .as_deref()
                    .is_some_and(|filter| expression_may_return_set(engine, filter))
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => items
            .iter()
            .any(|item| expression_may_return_set(engine, item)),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expression_may_return_set(engine, lhs) || expression_may_return_set(engine, rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expression_may_return_set(engine, inner),
        ScalarExpr::Between { expr, low, high } => {
            expression_may_return_set(engine, expr)
                || expression_may_return_set(engine, low)
                || expression_may_return_set(engine, high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expression_may_return_set(engine, expr)
                || list
                    .iter()
                    .any(|item| expression_may_return_set(engine, item))
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter()
                .any(|argument| expression_may_return_set(engine, argument))
                || spec
                    .partition_by
                    .iter()
                    .any(|item| expression_may_return_set(engine, item))
                || spec
                    .order_by
                    .iter()
                    .any(|order| expression_may_return_set(engine, &order.expr))
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_may_return_set(engine, &frame.start)
                        || frame_bound_may_return_set(engine, &frame.end)
                })
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref()
                .is_some_and(|base| expression_may_return_set(engine, base))
                || when.iter().any(|(condition, result)| {
                    expression_may_return_set(engine, condition)
                        || expression_may_return_set(engine, result)
                })
                || else_branch
                    .as_deref()
                    .is_some_and(|branch| expression_may_return_set(engine, branch))
        }
        ScalarExpr::InSubquery { expr, .. } => expression_may_return_set(engine, expr),
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

fn frame_bound_may_return_set(engine: &Engine, bound: &ScalarFrameBound) -> bool {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            expression_may_return_set(engine, expression)
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => false,
    }
}

fn frame_bound_expression(bound: &ScalarFrameBound) -> Option<&ScalarExpr> {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            Some(expression)
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => None,
    }
}

fn set_context_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "0A000".into(),
        message: message.into(),
    }
}

fn reject_set_descendant<'a>(
    engine: &Engine,
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    message: &str,
) -> Result<(), SQLError> {
    if expressions
        .into_iter()
        .any(|expression| expression_may_return_set(engine, expression))
    {
        return Err(set_context_error(message));
    }
    Ok(())
}

fn validate_set_context(engine: &Engine, expression: &ScalarExpr) -> Result<(), SQLError> {
    match expression {
        ScalarExpr::Func {
            name,
            args,
            order_by,
            filter,
            ..
        } => {
            let lower = crate::sql::builtin_function_dispatch_name(name);
            if is_aggregate(engine, expression) {
                reject_set_descendant(
                    engine,
                    args.iter()
                        .chain(order_by.iter().map(|order| &order.expr))
                        .chain(filter.iter().map(AsRef::as_ref)),
                    "aggregate function calls cannot contain set-returning function calls",
                )?;
            } else if lower == "coalesce" {
                reject_set_descendant(
                    engine,
                    args,
                    "set-returning functions are not allowed in COALESCE",
                )?;
            }
            for argument in args {
                validate_set_context(engine, argument)?;
            }
            for order in order_by {
                validate_set_context(engine, &order.expr)?;
            }
            if let Some(filter) = filter {
                validate_set_context(engine, filter)?;
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            reject_set_descendant(
                engine,
                args.iter()
                    .chain(spec.partition_by.iter())
                    .chain(spec.order_by.iter().map(|order| &order.expr))
                    .chain(
                        spec.frame
                            .iter()
                            .flat_map(|frame| [&frame.start, &frame.end])
                            .filter_map(|bound| frame_bound_expression(bound)),
                    ),
                "window function calls cannot contain set-returning function calls",
            )?;
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            let descendants = base
                .iter()
                .map(AsRef::as_ref)
                .chain(
                    when.iter()
                        .flat_map(|(condition, result)| [condition, result]),
                )
                .chain(else_branch.iter().map(AsRef::as_ref));
            reject_set_descendant(
                engine,
                descendants,
                "set-returning functions are not allowed in CASE",
            )?;
        }
        ScalarExpr::Not(inner) => {
            reject_set_descendant(
                engine,
                [inner.as_ref()],
                "argument of NOT must not return a set",
            )?;
        }
        ScalarExpr::And(items) => {
            reject_set_descendant(engine, items, "argument of AND must not return a set")?;
        }
        ScalarExpr::Or(items) | ScalarExpr::InList { list: items, .. } => {
            reject_set_descendant(engine, items, "argument of OR must not return a set")?;
            if let ScalarExpr::InList { expr, .. } = expression {
                reject_set_descendant(
                    engine,
                    [expr.as_ref()],
                    "argument of OR must not return a set",
                )?;
            }
        }
        ScalarExpr::Between { expr, low, high } => {
            reject_set_descendant(
                engine,
                [expr.as_ref(), low.as_ref(), high.as_ref()],
                "argument of AND must not return a set",
            )?;
        }
        ScalarExpr::Array(items) => {
            for item in items {
                validate_set_context(engine, item)?;
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            validate_set_context(engine, lhs)?;
            validate_set_context(engine, rhs)?;
        }
        ScalarExpr::IsNull { expr, .. } | ScalarExpr::Cast { expr, .. } => {
            validate_set_context(engine, expr)?;
        }
        ScalarExpr::InSubquery { expr, .. } => {
            reject_set_descendant(
                engine,
                [expr.as_ref()],
                "set-returning functions are not allowed in IN",
            )?;
        }
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
    Ok(())
}

pub(in crate::sql) fn validate_projection_set_contexts(
    engine: &Engine,
    projections: &[ProjectionPlan],
) -> Result<(), SQLError> {
    for projection in projections {
        validate_set_context(engine, &projection.expr)?;
    }
    Ok(())
}

pub(in crate::sql) fn validate_query_set_contexts(
    engine: &Engine,
    statement: &QueryBlockPlan,
) -> Result<(), SQLError> {
    validate_projection_set_contexts(engine, &statement.projections)?;
    for expression in statement
        .group_by
        .iter()
        .chain(statement.grouping_sets.iter().flatten())
        .chain(statement.order_by.iter().map(|order| &order.expr))
        .chain(statement.distinct_on.iter())
    {
        validate_set_context(engine, expression)?;
    }
    if let Some(predicate) = statement.r#where.as_ref() {
        reject_set_descendant(
            engine,
            [predicate],
            "set-returning functions are not allowed in WHERE",
        )?;
    }
    if let Some(having) = statement.having.as_ref() {
        reject_set_descendant(
            engine,
            [having],
            "set-returning functions are not allowed in HAVING",
        )?;
    }
    if let Some(limit) = statement.limit.as_ref() {
        reject_set_descendant(
            engine,
            [limit],
            "set-returning functions are not allowed in LIMIT",
        )?;
    }
    if let Some(offset) = statement.offset.as_ref() {
        reject_set_descendant(
            engine,
            [offset],
            "set-returning functions are not allowed in OFFSET",
        )?;
    }
    if let Some(source) = statement.from.as_ref() {
        validate_source_set_contexts(engine, source)?;
    }
    Ok(())
}

fn validate_source_set_contexts(engine: &Engine, source: &SourcePlan) -> Result<(), SQLError> {
    match source {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            validate_source_set_contexts(engine, left)?;
            validate_source_set_contexts(engine, right)?;
            if let Some(condition) = on {
                reject_set_descendant(
                    engine,
                    [condition],
                    "set-returning functions are not allowed in JOIN conditions",
                )?;
            }
        }
        SourcePlan::Values { rows, .. } => validate_values_set_contexts(engine, rows)?,
        SourcePlan::Function { args, .. } => {
            reject_set_descendant(
                engine,
                args,
                "set-returning functions must appear at top level of FROM",
            )?;
        }
        SourcePlan::Subquery { .. } | SourcePlan::Table { .. } => {}
    }
    Ok(())
}

pub(in crate::sql) fn validate_values_set_contexts(
    engine: &Engine,
    rows: &[Vec<ScalarExpr>],
) -> Result<(), SQLError> {
    reject_set_descendant(
        engine,
        rows.iter().flatten(),
        "set-returning functions are not allowed in VALUES",
    )
}

fn rewrite_set_calls(
    engine: &Engine,
    mut expression: ScalarExpr,
    calls: &mut Vec<SetFunctionCall>,
) -> ScalarExpr {
    let descendant_start = calls.len();
    match &mut expression {
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                *argument = rewrite_set_calls(engine, argument.clone(), calls);
            }
            for order in order_by {
                order.expr = rewrite_set_calls(engine, order.expr.clone(), calls);
            }
            if let Some(filter) = filter {
                **filter = rewrite_set_calls(engine, (**filter).clone(), calls);
            }
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                *item = rewrite_set_calls(engine, item.clone(), calls);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            **lhs = rewrite_set_calls(engine, (**lhs).clone(), calls);
            **rhs = rewrite_set_calls(engine, (**rhs).clone(), calls);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            **inner = rewrite_set_calls(engine, (**inner).clone(), calls);
        }
        ScalarExpr::Between { expr, low, high } => {
            **expr = rewrite_set_calls(engine, (**expr).clone(), calls);
            **low = rewrite_set_calls(engine, (**low).clone(), calls);
            **high = rewrite_set_calls(engine, (**high).clone(), calls);
        }
        ScalarExpr::InList { expr, list, .. } => {
            **expr = rewrite_set_calls(engine, (**expr).clone(), calls);
            for item in list {
                *item = rewrite_set_calls(engine, item.clone(), calls);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for argument in args {
                *argument = rewrite_set_calls(engine, argument.clone(), calls);
            }
            for item in &mut spec.partition_by {
                *item = rewrite_set_calls(engine, item.clone(), calls);
            }
            for order in &mut spec.order_by {
                order.expr = rewrite_set_calls(engine, order.expr.clone(), calls);
            }
            if let Some(frame) = &mut spec.frame {
                rewrite_set_frame_bound(engine, &mut frame.start, calls);
                rewrite_set_frame_bound(engine, &mut frame.end, calls);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                **base = rewrite_set_calls(engine, (**base).clone(), calls);
            }
            for (condition, result) in when {
                *condition = rewrite_set_calls(engine, condition.clone(), calls);
                *result = rewrite_set_calls(engine, result.clone(), calls);
            }
            if let Some(branch) = else_branch {
                **branch = rewrite_set_calls(engine, (**branch).clone(), calls);
            }
        }
        ScalarExpr::InSubquery { expr, .. } => {
            **expr = rewrite_set_calls(engine, (**expr).clone(), calls);
        }
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
    if let ScalarExpr::Func { name, args, .. } = &expression {
        if function_may_return_set(engine, name) {
            let level = calls[descendant_start..]
                .iter()
                .map(|call| call.level + 1)
                .max()
                .unwrap_or(0);
            let placeholder = format!("{SET_VALUE_COLUMN_PREFIX}{}", calls.len());
            calls.push(SetFunctionCall {
                placeholder: placeholder.clone(),
                name: name.clone(),
                args: args.clone(),
                level,
            });
            return ScalarExpr::Column(placeholder);
        }
    }
    expression
}

fn rewrite_set_frame_bound(
    engine: &Engine,
    bound: &mut ScalarFrameBound,
    calls: &mut Vec<SetFunctionCall>,
) {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            **expression = rewrite_set_calls(engine, (**expression).clone(), calls);
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
}

fn replace_group_set_expression(expression: &mut ScalarExpr, mappings: &[(ScalarExpr, String)]) {
    if let Some((_, column)) = mappings
        .iter()
        .find(|(group, _)| exprs_match(expression, group))
    {
        *expression = ScalarExpr::Column(column.clone());
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
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                replace_group_set_expression(item, mappings);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            replace_group_set_expression(lhs, mappings);
            replace_group_set_expression(rhs, mappings);
        }
        ScalarExpr::Not(inner)
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
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
}

fn replace_group_set_frame_bound(bound: &mut ScalarFrameBound, mappings: &[(ScalarExpr, String)]) {
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
    statement: &QueryBlockPlan,
) -> Option<GroupSetProjectionPlan> {
    let mut groups = Vec::new();
    for expression in statement
        .group_by
        .iter()
        .chain(statement.grouping_sets.iter().flatten())
    {
        if expression_may_return_set(engine, expression)
            && !groups
                .iter()
                .any(|existing| exprs_match(existing, expression))
        {
            groups.push(expression.clone());
        }
    }
    if groups.is_empty() {
        return None;
    }

    let mappings = groups
        .iter()
        .enumerate()
        .map(|(index, expression)| (expression.clone(), format!("\0uqa.group_set_value.{index}")))
        .collect::<Vec<_>>();
    let projections = mappings
        .iter()
        .map(|(expression, column)| (column.clone(), expression.clone()))
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
    Some(GroupSetProjectionPlan {
        statement: rewritten,
        projections,
    })
}

fn capture_aggregate_dependency(
    expression: &ScalarExpr,
    dependencies: &mut Vec<ProjectionPlan>,
) -> ScalarExpr {
    let placeholder = format!("\0uqa.aggregate_output.{}", dependencies.len());
    dependencies.push(ProjectionPlan {
        expr: expression.clone(),
        alias: Some(placeholder.clone()),
    });
    ScalarExpr::Column(placeholder)
}

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
        ScalarExpr::Column(_) | ScalarExpr::QualifiedColumn { .. } => {
            capture_aggregate_dependency(expression, dependencies)
        }
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
            name: name.clone(),
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
        ScalarExpr::Star
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

pub(in crate::sql) fn prepare_aggregate_set_projection(
    engine: &Engine,
    statement: &QueryBlockPlan,
) -> Option<AggregateSetProjectionPlan> {
    let physical = statement
        .projections
        .iter()
        .zip(projection_columns(&statement.projections))
        .map(|(projection, label)| (label, projection.expr.clone()))
        .collect::<Vec<_>>();
    if !projections_may_return_set(engine, &physical) {
        return None;
    }

    let labels = projection_columns(&statement.projections);
    let mut dependencies = Vec::new();
    let projections = statement
        .projections
        .iter()
        .zip(labels)
        .map(|(projection, label)| {
            (
                label,
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
            alias: Some("\0uqa.aggregate_output.seed".into()),
        });
    }
    let mut aggregate_statement = statement.clone();
    aggregate_statement.projections = dependencies;
    Some(AggregateSetProjectionPlan {
        statement: aggregate_statement,
        projections,
    })
}

impl SetProjectionPlan {
    fn new(
        engine: &Engine,
        projections: Vec<PhysicalProjection>,
        output_batch_size: usize,
    ) -> Self {
        let mut calls = Vec::new();
        let projections = projections
            .into_iter()
            .map(|(name, expression)| (name, rewrite_set_calls(engine, expression, &mut calls)))
            .collect();
        debug_assert!(!calls.is_empty());
        Self {
            projections,
            calls,
            output_batch_size,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::sql) fn build_set_projection<'a>(
    mut operator: Box<dyn PhysicalOperator + 'a>,
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    evaluator: SharedExpressionEvaluator<'a>,
    projections: Vec<PhysicalProjection>,
    pass_through: bool,
    output_batch_size: usize,
) -> Box<dyn PhysicalOperator + 'a> {
    let plan = SetProjectionPlan::new(engine, projections, output_batch_size);
    let max_level = plan.calls.iter().map(|call| call.level).max().unwrap_or(0);
    for level in 0..=max_level {
        let calls = plan
            .calls
            .iter()
            .filter(|call| call.level == level)
            .cloned()
            .collect::<Vec<_>>();
        if calls.is_empty() {
            continue;
        }
        let projections = calls
            .iter()
            .map(|call| {
                (
                    call.placeholder.clone(),
                    ScalarExpr::Column(call.placeholder.clone()),
                )
            })
            .collect();
        operator = Box::new(SetProjection::from_plan(
            operator,
            engine,
            params,
            ctes,
            evaluator.clone(),
            SetProjectionPlan {
                projections,
                calls,
                output_batch_size: plan.output_batch_size,
            },
            true,
        ));
    }
    if pass_through {
        Box::new(Project::appending_with_evaluator(
            operator,
            plan.projections,
            evaluator,
        ))
    } else {
        Box::new(Project::with_evaluator(
            operator,
            plan.projections,
            evaluator,
        ))
    }
}

pub(in crate::sql) struct SetProjection<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    evaluator: SharedExpressionEvaluator<'a>,
    plan: SetProjectionPlan,
    schema: RowSchema,
    pass_through: bool,
    output_batch_size: usize,
    input: std::vec::IntoIter<ResultRow>,
    expansion: Option<SetExpansion>,
    exhausted: bool,
}

impl<'a> SetProjection<'a> {
    fn from_plan(
        child: Box<dyn PhysicalOperator + 'a>,
        engine: &'a Engine,
        params: &'a [SQLParam],
        ctes: &'a CteScope,
        evaluator: SharedExpressionEvaluator<'a>,
        plan: SetProjectionPlan,
        pass_through: bool,
    ) -> Self {
        let projections = &plan.projections;
        let schema = if pass_through {
            let appended = projections
                .iter()
                .filter(|(_, expression)| !matches!(expression, ScalarExpr::Star))
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            RowSchema::append(child.row_schema(), &appended)
        } else {
            let mut columns = Vec::new();
            for (name, expression) in projections {
                if matches!(expression, ScalarExpr::Star) {
                    for column in child.schema() {
                        if !columns.contains(column) {
                            columns.push(column.clone());
                        }
                    }
                } else {
                    columns.push(name.clone());
                }
            }
            RowSchema::new(columns)
        };
        let output_batch_size = plan.output_batch_size.max(1);
        Self {
            child,
            engine,
            params,
            ctes,
            evaluator,
            plan,
            schema,
            pass_through,
            output_batch_size,
            input: Vec::new().into_iter(),
            expansion: None,
            exhausted: false,
        }
    }

    fn next_input(&mut self) -> ExecResult<Option<ResultRow>> {
        loop {
            if let Some(row) = self.input.next() {
                return Ok(Some(row));
            }
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            self.input = batch.into_result_rows().into_iter();
        }
    }

    fn call_state(&self, call: &SetFunctionCall, row: &ResultRow) -> ExecResult<SetFunctionState> {
        let identity = call.name.to_ascii_lowercase();
        if !self.engine.has_registered_table_function(&identity)
            && self.engine.lookup_sql_functions(&call.name).is_some()
        {
            let hook = ScopedEngineHook::new(self.engine, self.ctes);
            let subqueries = PlanSubqueryArena::new(&self.ctes.scalar_subqueries, Some(&hook));
            let context = ScalarEvalContext::new(Some(row), self.params)
                .with_function_hook(&hook)
                .with_subquery_runner(&subqueries);
            let arguments = eval_call_arguments(&call.args, &context)?;
            let returns_set = crate::sql::plpgsql_exec::resolved_user_function_returns_set(
                self.engine,
                &call.name,
                &arguments,
            )
            .ok_or_else(|| {
                uqa_execution::ExecError::Other(format!(
                    "user function `{}` disappeared during projection",
                    call.name
                ))
            })??;
            if !returns_set {
                let value = crate::sql::plpgsql_exec::call_user_scalar_function(
                    self.engine,
                    &call.name,
                    &arguments,
                )
                .ok_or_else(|| {
                    uqa_execution::ExecError::Other(format!(
                        "user function `{}` disappeared during scalar projection",
                        call.name
                    ))
                })??;
                return Ok(SetFunctionState::Scalar(value));
            }
            let result = crate::sql::plpgsql_exec::call_user_table_function(
                self.engine,
                &call.name,
                &arguments,
            )
            .ok_or_else(|| {
                uqa_execution::ExecError::Other(format!(
                    "user function `{}` disappeared during set projection",
                    call.name
                ))
            })??;
            let rows = crate::sql::from_rows::registered_table_function_rows(
                &call.name,
                result,
                None,
                &[],
            )?;
            return Ok(SetFunctionState::Set {
                rows: Box::new(rows.into_iter().map(Ok)),
                exhausted: false,
            });
        }

        let hook = ScopedEngineHook::new(self.engine, self.ctes);
        let context = crate::sql::from_rows::TableFunctionEvalContext::new(
            self.engine,
            self.params,
            &hook,
            &hook,
            &self.ctes.scalar_subqueries,
        );
        let table_call = crate::sql::from_rows::TableFunctionCall::new(
            &call.name,
            None,
            &call.args,
            None,
            &[],
            &[],
        );
        let rows = crate::sql::from_rows::build_table_function_row_stream_with_row(
            &context,
            table_call,
            Some(row),
        )?;
        Ok(SetFunctionState::Set {
            rows,
            exhausted: false,
        })
    }

    fn start_expansion(&self, input: ResultRow) -> ExecResult<SetExpansion> {
        let calls = self
            .plan
            .calls
            .iter()
            .map(|call| self.call_state(call, &input))
            .collect::<ExecResult<Vec<_>>>()?;
        let has_set = calls
            .iter()
            .any(|call| matches!(call, SetFunctionState::Set { .. }));
        Ok(SetExpansion {
            input,
            calls,
            has_set,
            scalar_emitted: false,
        })
    }

    fn next_projected(&mut self) -> ExecResult<Option<ResultRow>> {
        let Some(expansion) = self.expansion.as_mut() else {
            return Ok(None);
        };
        let Some(values) = expansion.next_values()? else {
            return Ok(None);
        };
        let mut evaluation_row = expansion.input.clone();
        for (call, value) in self.plan.calls.iter().zip(values) {
            evaluation_row.insert(call.placeholder.clone(), value);
        }
        let mut output = if self.pass_through {
            expansion.input.clone()
        } else {
            ResultRow::new()
        };
        for (name, expression) in &self.plan.projections {
            if matches!(expression, ScalarExpr::Star) {
                if !self.pass_through {
                    output.extend(self.evaluator.project_star(&expansion.input)?);
                }
            } else {
                output.insert(
                    name.clone(),
                    self.evaluator.evaluate(expression, &evaluation_row)?,
                );
            }
        }
        Ok(Some(output))
    }
}

impl PhysicalOperator for SetProjection<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.input = Vec::new().into_iter();
        self.expansion = None;
        self.exhausted = false;
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.exhausted && self.expansion.is_none() {
            return Ok(None);
        }
        let mut output = Vec::with_capacity(self.output_batch_size);
        while output.len() < self.output_batch_size {
            if let Some(row) = self.next_projected()? {
                output.push(row);
                continue;
            }
            self.expansion = None;
            if let Some(input) = self.next_input()? {
                self.expansion = Some(self.start_expansion(input)?);
            } else {
                self.exhausted = true;
                break;
            }
        }
        if output.is_empty() {
            return Ok(None);
        }
        Ok(Some(Batch::new(self.schema.clone(), output)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.input = Vec::new().into_iter();
        self.expansion = None;
        self.exhausted = true;
        self.child.close()
    }
}
