//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-style set-returning SELECT-list projection.

use uqa_core::Value;
use uqa_execution::{
    eval_call_arguments, Batch, ExecResult, OwnedPhysicalRow, PhysicalOperator, PhysicalRow,
    Project, ProjectRows, RowSchema, ScalarEvalContext, ScalarExpr, ScalarFrameBound,
    SharedExpressionEvaluator,
};
use uqa_planner::{ProjectionPlan, QueryBlockPlan, SourcePlan};
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::{ResultRow, SQLError, SQLParam};

use super::{projection_columns, CteScope, Engine, PhysicalProjection, ScopedEngineHook};
use crate::sql::aggregates::{exprs_match, is_aggregate};
use crate::sql::scalar::PlanSubqueryArena;

const SET_VALUE_COLUMN_PREFIX: &str = "\0uqa.set_value.";

#[derive(Clone)]
struct SetFunctionCall {
    placeholder: String,
    name: String,
    binding: Option<FunctionBinding>,
    args: Vec<ScalarExpr>,
    level: usize,
}

struct SetProjectionPlan {
    projections: Vec<PhysicalProjection>,
    calls: Vec<SetFunctionCall>,
    output_batch_size: usize,
}

pub(in crate::sql) struct AggregateOutputProjectionPlan {
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
    input: OwnedPhysicalRow,
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
    Value::Record(row.into_iter().collect())
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

fn function_may_return_set(
    engine: &Engine,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    let identity = name.to_ascii_lowercase();
    let builtin = crate::sql::builtin_function_dispatch_name(&identity);
    if builtin_returns_set(&builtin) || engine.has_registered_table_function(&identity) {
        return Ok(true);
    }
    if engine.lookup_sql_functions(name).is_none() {
        return Ok(false);
    }
    let mut argument_names = Vec::with_capacity(args.len());
    let mut argument_types = Vec::with_capacity(args.len());
    for argument in args {
        let (argument_name, value) = set_function_argument(argument);
        argument_names.push(argument_name);
        argument_types.push(uqa_execution::common_context_expression_type(
            value,
            schema,
            params,
            Some(engine),
        )?);
    }
    Ok(engine
        .resolve_static_sql_function(name, binding, &argument_names, &argument_types)?
        .is_some_and(|function| function.def.returns_set()))
}

fn set_function_argument(expression: &ScalarExpr) -> (Option<String>, &ScalarExpr) {
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

pub(in crate::sql) fn projections_may_return_set(
    engine: &Engine,
    projections: &[PhysicalProjection],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    for (_, expression) in projections {
        if expression_may_return_set(engine, expression, schema, params)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(in crate::sql) fn expression_may_return_set(
    engine: &Engine,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    match expression {
        ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            filter,
            ..
        } => {
            if function_may_return_set(engine, name, binding.as_ref(), args, schema, params)?
                || expressions_may_return_set(engine, args, schema, params)?
                || expressions_may_return_set(
                    engine,
                    order_by.iter().map(|order| &order.expr),
                    schema,
                    params,
                )?
            {
                return Ok(true);
            }
            filter.as_deref().map_or(Ok(false), |filter| {
                expression_may_return_set(engine, filter, schema, params)
            })
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => expressions_may_return_set(engine, items, schema, params),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            Ok(expression_may_return_set(engine, lhs, schema, params)?
                || expression_may_return_set(engine, rhs, schema, params)?)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            expression_may_return_set(engine, inner, schema, params)
        }
        ScalarExpr::Between { expr, low, high } => {
            Ok(expression_may_return_set(engine, expr, schema, params)?
                || expression_may_return_set(engine, low, schema, params)?
                || expression_may_return_set(engine, high, schema, params)?)
        }
        ScalarExpr::InList { expr, list, .. } => {
            Ok(expression_may_return_set(engine, expr, schema, params)?
                || expressions_may_return_set(engine, list, schema, params)?)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            if expressions_may_return_set(engine, args, schema, params)?
                || expressions_may_return_set(engine, &spec.partition_by, schema, params)?
                || expressions_may_return_set(
                    engine,
                    spec.order_by.iter().map(|order| &order.expr),
                    schema,
                    params,
                )?
            {
                return Ok(true);
            }
            let Some(frame) = spec.frame.as_ref() else {
                return Ok(false);
            };
            Ok(
                frame_bound_may_return_set(engine, &frame.start, schema, params)?
                    || frame_bound_may_return_set(engine, &frame.end, schema, params)?,
            )
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                if expression_may_return_set(engine, base, schema, params)? {
                    return Ok(true);
                }
            }
            for (condition, result) in when {
                if expression_may_return_set(engine, condition, schema, params)?
                    || expression_may_return_set(engine, result, schema, params)?
                {
                    return Ok(true);
                }
            }
            if let Some(branch) = else_branch {
                if expression_may_return_set(engine, branch, schema, params)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        ScalarExpr::InSubquery { expr, .. } => {
            expression_may_return_set(engine, expr, schema, params)
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => Ok(false),
    }
}

fn expressions_may_return_set<'a>(
    engine: &Engine,
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    for expression in expressions {
        if expression_may_return_set(engine, expression, schema, params)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn frame_bound_may_return_set(
    engine: &Engine,
    bound: &ScalarFrameBound,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            expression_may_return_set(engine, expression, schema, params)
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => Ok(false),
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
    schema: &RowSchema,
    params: &[SQLParam],
    message: &str,
) -> Result<(), SQLError> {
    if expressions_may_return_set(engine, expressions, schema, params)? {
        return Err(set_context_error(message));
    }
    Ok(())
}

fn validate_set_context(
    engine: &Engine,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
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
                    schema,
                    params,
                    "aggregate function calls cannot contain set-returning function calls",
                )?;
            } else if lower == "coalesce" {
                reject_set_descendant(
                    engine,
                    args,
                    schema,
                    params,
                    "set-returning functions are not allowed in COALESCE",
                )?;
            }
            for argument in args {
                validate_set_context(engine, argument, schema, params)?;
            }
            for order in order_by {
                validate_set_context(engine, &order.expr, schema, params)?;
            }
            if let Some(filter) = filter {
                validate_set_context(engine, filter, schema, params)?;
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
                schema,
                params,
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
                schema,
                params,
                "set-returning functions are not allowed in CASE",
            )?;
        }
        ScalarExpr::Not(inner) => {
            reject_set_descendant(
                engine,
                [inner.as_ref()],
                schema,
                params,
                "argument of NOT must not return a set",
            )?;
        }
        ScalarExpr::UnaryMinus(inner) => validate_set_context(engine, inner, schema, params)?,
        ScalarExpr::And(items) => {
            reject_set_descendant(
                engine,
                items,
                schema,
                params,
                "argument of AND must not return a set",
            )?;
        }
        ScalarExpr::Or(items) => {
            reject_set_descendant(
                engine,
                items,
                schema,
                params,
                "argument of OR must not return a set",
            )?;
        }
        ScalarExpr::InList { expr, list, .. } => {
            validate_set_context(engine, expr, schema, params)?;
            reject_set_descendant(
                engine,
                list,
                schema,
                params,
                "argument of IN must not return a set",
            )?;
        }
        ScalarExpr::Between { expr, low, high } => {
            reject_set_descendant(
                engine,
                [expr.as_ref(), low.as_ref(), high.as_ref()],
                schema,
                params,
                "argument of AND must not return a set",
            )?;
        }
        ScalarExpr::Array(items) | ScalarExpr::Row(items) => {
            for item in items {
                validate_set_context(engine, item, schema, params)?;
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            validate_set_context(engine, lhs, schema, params)?;
            validate_set_context(engine, rhs, schema, params)?;
        }
        ScalarExpr::IsNull { expr, .. } | ScalarExpr::Cast { expr, .. } => {
            validate_set_context(engine, expr, schema, params)?;
        }
        ScalarExpr::InSubquery { expr, .. } => {
            reject_set_descendant(
                engine,
                [expr.as_ref()],
                schema,
                params,
                "row comparison operator must not return a set",
            )?;
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
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
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for projection in projections {
        validate_set_context(engine, &projection.expr, schema, params)?;
    }
    Ok(())
}

pub(in crate::sql) fn validate_query_set_contexts(
    engine: &Engine,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    validate_projection_set_contexts(engine, &statement.projections, schema, params)?;
    if let Some(locking) = statement.locking.first() {
        for projection in &statement.projections {
            if expression_may_return_set(engine, &projection.expr, schema, params)? {
                return Err(SQLError::Unsupported(format!(
                    "{} is not allowed with set-returning functions in the target list",
                    locking.strength.sql_name()
                )));
            }
        }
    }
    for expression in statement
        .group_by
        .iter()
        .chain(statement.grouping_sets.iter().flatten())
        .chain(statement.order_by.iter().map(|order| &order.expr))
        .chain(statement.distinct_on.iter())
    {
        validate_set_context(engine, expression, schema, params)?;
    }
    if let Some(predicate) = statement.r#where.as_ref() {
        reject_set_descendant(
            engine,
            [predicate],
            schema,
            params,
            "set-returning functions are not allowed in WHERE",
        )?;
    }
    if let Some(having) = statement.having.as_ref() {
        reject_set_descendant(
            engine,
            [having],
            schema,
            params,
            "set-returning functions are not allowed in HAVING",
        )?;
    }
    if let Some(limit) = statement.limit.as_ref() {
        reject_set_descendant(
            engine,
            [limit],
            schema,
            params,
            "set-returning functions are not allowed in LIMIT",
        )?;
    }
    if let Some(offset) = statement.offset.as_ref() {
        reject_set_descendant(
            engine,
            [offset],
            schema,
            params,
            "set-returning functions are not allowed in OFFSET",
        )?;
    }
    if let Some(source) = statement.from.as_ref() {
        validate_source_set_contexts(engine, source, schema, params)?;
    }
    Ok(())
}

fn validate_source_set_contexts(
    engine: &Engine,
    source: &SourcePlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            validate_source_set_contexts(engine, left, schema, params)?;
            validate_source_set_contexts(engine, right, schema, params)?;
            if let Some(condition) = on {
                reject_set_descendant(
                    engine,
                    [condition],
                    schema,
                    params,
                    "set-returning functions are not allowed in JOIN conditions",
                )?;
            }
        }
        SourcePlan::Values { rows, .. } => {
            validate_values_set_contexts(engine, rows, schema, params)?;
        }
        SourcePlan::Function { args, .. } => {
            reject_set_descendant(
                engine,
                args,
                schema,
                params,
                "set-returning functions must appear at top level of FROM",
            )?;
        }
        SourcePlan::Subquery { .. } | SourcePlan::Table { .. } => {}
    }
    Ok(())
}

pub(in crate::sql) fn validate_source_set_contexts_before_build(
    engine: &Engine,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Join {
            left,
            right,
            lateral,
            ..
        } => {
            validate_source_set_contexts_before_build(engine, left, params, ctes, outer)?;
            let left_schema = super::bind_source_plan_schema(engine, left, params, ctes, outer)?;
            let implicit_lateral_function = matches!(right.as_ref(), SourcePlan::Function { .. });
            let right_scope = (*lateral || implicit_lateral_function)
                .then(|| overlay_set_validation_scope(&left_schema, outer));
            validate_source_set_contexts_before_build(
                engine,
                right,
                params,
                ctes,
                right_scope.as_ref().or(outer),
            )
        }
        SourcePlan::Values { rows, .. } => {
            let empty = RowSchema::default();
            validate_values_set_contexts(engine, rows, outer.unwrap_or(&empty), params)
        }
        SourcePlan::Function { args, .. } => {
            let empty = RowSchema::default();
            reject_set_descendant(
                engine,
                args,
                outer.unwrap_or(&empty),
                params,
                "set-returning functions must appear at top level of FROM",
            )
        }
        SourcePlan::Subquery { .. } | SourcePlan::Table { .. } => Ok(()),
    }
}

fn overlay_set_validation_scope(current: &RowSchema, outer: Option<&RowSchema>) -> RowSchema {
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

pub(in crate::sql) fn validate_values_set_contexts(
    engine: &Engine,
    rows: &[Vec<ScalarExpr>],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    reject_set_descendant(
        engine,
        rows.iter().flatten(),
        schema,
        params,
        "set-returning functions are not allowed in VALUES",
    )
}

fn rewrite_set_calls(
    engine: &Engine,
    mut expression: ScalarExpr,
    calls: &mut Vec<SetFunctionCall>,
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
                *argument = rewrite_set_calls(engine, argument.clone(), calls, schema, params)?;
            }
            for order in order_by {
                order.expr = rewrite_set_calls(engine, order.expr.clone(), calls, schema, params)?;
            }
            if let Some(filter) = filter {
                **filter = rewrite_set_calls(engine, (**filter).clone(), calls, schema, params)?;
            }
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            for item in items {
                *item = rewrite_set_calls(engine, item.clone(), calls, schema, params)?;
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            **lhs = rewrite_set_calls(engine, (**lhs).clone(), calls, schema, params)?;
            **rhs = rewrite_set_calls(engine, (**rhs).clone(), calls, schema, params)?;
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            **inner = rewrite_set_calls(engine, (**inner).clone(), calls, schema, params)?;
        }
        ScalarExpr::Between { expr, low, high } => {
            **expr = rewrite_set_calls(engine, (**expr).clone(), calls, schema, params)?;
            **low = rewrite_set_calls(engine, (**low).clone(), calls, schema, params)?;
            **high = rewrite_set_calls(engine, (**high).clone(), calls, schema, params)?;
        }
        ScalarExpr::InList { expr, list, .. } => {
            **expr = rewrite_set_calls(engine, (**expr).clone(), calls, schema, params)?;
            for item in list {
                *item = rewrite_set_calls(engine, item.clone(), calls, schema, params)?;
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for argument in args {
                *argument = rewrite_set_calls(engine, argument.clone(), calls, schema, params)?;
            }
            for item in &mut spec.partition_by {
                *item = rewrite_set_calls(engine, item.clone(), calls, schema, params)?;
            }
            for order in &mut spec.order_by {
                order.expr = rewrite_set_calls(engine, order.expr.clone(), calls, schema, params)?;
            }
            if let Some(frame) = &mut spec.frame {
                rewrite_set_frame_bound(engine, &mut frame.start, calls, schema, params)?;
                rewrite_set_frame_bound(engine, &mut frame.end, calls, schema, params)?;
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                **base = rewrite_set_calls(engine, (**base).clone(), calls, schema, params)?;
            }
            for (condition, result) in when {
                *condition = rewrite_set_calls(engine, condition.clone(), calls, schema, params)?;
                *result = rewrite_set_calls(engine, result.clone(), calls, schema, params)?;
            }
            if let Some(branch) = else_branch {
                **branch = rewrite_set_calls(engine, (**branch).clone(), calls, schema, params)?;
            }
        }
        ScalarExpr::InSubquery { expr, .. } => {
            **expr = rewrite_set_calls(engine, (**expr).clone(), calls, schema, params)?;
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
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
        if function_may_return_set(engine, name, binding.as_ref(), args, schema, params)? {
            let level = calls[descendant_start..]
                .iter()
                .map(|call| call.level + 1)
                .max()
                .unwrap_or(0);
            let placeholder = format!("{SET_VALUE_COLUMN_PREFIX}{}", calls.len());
            calls.push(SetFunctionCall {
                placeholder: placeholder.clone(),
                name: name.clone(),
                binding: binding.clone(),
                args: args.clone(),
                level,
            });
            return Ok(ScalarExpr::Column(placeholder));
        }
    }
    Ok(expression)
}

fn rewrite_set_frame_bound(
    engine: &Engine,
    bound: &mut ScalarFrameBound,
    calls: &mut Vec<SetFunctionCall>,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            **expression =
                rewrite_set_calls(engine, (**expression).clone(), calls, schema, params)?;
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
    Ok(())
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
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<GroupSetProjectionPlan>, SQLError> {
    let mut groups = Vec::new();
    for expression in statement
        .group_by
        .iter()
        .chain(statement.grouping_sets.iter().flatten())
    {
        if expression_may_return_set(engine, expression, schema, params)?
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
    Ok(Some(GroupSetProjectionPlan {
        statement: rewritten,
        projections,
    }))
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
        ScalarExpr::Column(_) | ScalarExpr::Position(_) | ScalarExpr::QualifiedColumn { .. } => {
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
) -> AggregateOutputProjectionPlan {
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
    AggregateOutputProjectionPlan {
        statement: aggregate_statement,
        projections,
    }
}

impl SetProjectionPlan {
    fn new(
        engine: &Engine,
        projections: Vec<PhysicalProjection>,
        schema: &RowSchema,
        params: &[SQLParam],
        output_batch_size: usize,
    ) -> Result<Self, SQLError> {
        let mut calls = Vec::new();
        let projections = projections
            .into_iter()
            .map(|(name, expression)| {
                Ok((
                    name,
                    rewrite_set_calls(engine, expression, &mut calls, schema, params)?,
                ))
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        debug_assert!(!calls.is_empty());
        Ok(Self {
            projections,
            calls,
            output_batch_size,
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::sql) fn build_set_projection<'a>(
    mut operator: Box<dyn PhysicalOperator + 'a>,
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: &CteScope,
    evaluator: SharedExpressionEvaluator<'a>,
    projections: Vec<PhysicalProjection>,
    pass_through: bool,
    output_batch_size: usize,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let plan = SetProjectionPlan::new(
        engine,
        projections,
        operator.row_schema(),
        params,
        output_batch_size,
    )?;
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
        Ok(Box::new(Project::appending_with_evaluator(
            operator,
            plan.projections,
            evaluator,
        )))
    } else {
        Ok(Box::new(Project::with_evaluator(
            operator,
            plan.projections,
            evaluator,
        )))
    }
}

mod operator;
pub(in crate::sql) use operator::SetProjection;
