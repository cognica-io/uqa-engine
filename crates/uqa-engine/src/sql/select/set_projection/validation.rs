//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-returning expression detection and SQL context validation.

use uqa_execution::{FunctionTypeResolver, RowSchema, ScalarExpr, ScalarFrameBound};
use uqa_planner::{ProjectionPlan, QueryBlockPlan, SourcePlan};
use uqa_sql::ast::FunctionBinding;
use uqa_sql::{SQLError, SQLParam};

use super::{CteScope, Engine, PhysicalProjection};
use crate::sql::aggregates::is_aggregate;

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

pub(super) fn function_may_return_set(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    if matches!(
        name,
        uqa_sql::expr::NAMED_ARG_FUNCTION | uqa_sql::expr::VARIADIC_ARG_FUNCTION
    ) {
        return Ok(false);
    }
    if binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax) {
        return Ok(false);
    }
    if binding.is_some_and(|binding| !binding.builtin) {
        let (argument_names, argument_types, explicit_variadic) =
            uqa_execution::function_call_argument_signature(args, schema, params, Some(resolver))?;
        let function = engine.resolve_static_sql_function(
            name,
            binding,
            &argument_names,
            &argument_types,
            explicit_variadic,
        )?;
        return Ok(function.is_some_and(|function| function.def.returns_set()));
    }
    let identity = name.to_ascii_lowercase();
    let builtin = crate::sql::builtin_function_dispatch_name(&identity);
    if builtin_returns_set(&builtin) || engine.has_registered_table_function(&identity) {
        return Ok(true);
    }
    let Some(overloads) = engine.lookup_sql_functions(name) else {
        return Ok(false);
    };
    if !uqa_execution::is_fixed_builtin(name) {
        let mut setness = overloads
            .iter()
            .filter(|function| !function.def.is_procedure)
            .map(|function| function.def.returns_set());
        if let Some(first) = setness.next() {
            if setness.all(|returns_set| returns_set == first) {
                return Ok(first);
            }
        }
    }
    let (argument_names, argument_types, explicit_variadic) =
        uqa_execution::function_call_argument_signature(args, schema, params, Some(resolver))?;
    if let Some(resolved) = uqa_execution::resolve_fixed_builtin_call(
        name,
        binding,
        &argument_names,
        &argument_types,
        explicit_variadic,
        Some(resolver),
    )? {
        if resolved.selected.binding.builtin {
            return Ok(false);
        }
        let function = engine.resolve_static_sql_function(
            name,
            Some(&resolved.selected.binding),
            &argument_names,
            &argument_types,
            explicit_variadic,
        )?;
        return Ok(function.is_some_and(|function| function.def.returns_set()));
    }
    match engine.resolve_static_sql_function(
        name,
        binding,
        &argument_names,
        &argument_types,
        explicit_variadic,
    ) {
        Ok(function) => Ok(function.is_some_and(|function| function.def.returns_set())),
        Err(error) if binding.is_none() && error.sqlstate() == Some("42883") => {
            match uqa_execution::type_resolution::builtin_function_type(
                &builtin,
                args,
                &[],
                schema,
                params,
            ) {
                Ok(Some(_)) => Ok(false),
                Ok(None) | Err(_) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

pub(super) fn resolve_set_function_binding(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<FunctionBinding>, SQLError> {
    if engine.lookup_sql_functions(name).is_none() {
        return Ok(binding.cloned());
    }
    let (argument_names, argument_types, explicit_variadic) =
        uqa_execution::function_call_argument_signature(args, schema, params, Some(resolver))?;
    if let Some(resolved) = uqa_execution::resolve_fixed_builtin_call(
        name,
        binding,
        &argument_names,
        &argument_types,
        explicit_variadic,
        Some(resolver),
    )? {
        return Ok((!resolved.selected.binding.builtin).then_some(resolved.selected.binding));
    }
    let Some(function) = engine.resolve_static_sql_function_match(
        name,
        binding,
        &argument_names,
        &argument_types,
        explicit_variadic,
    )?
    else {
        return Ok(binding.cloned());
    };
    Ok(Some(function.binding()))
}

pub(in crate::sql) fn projections_may_return_set(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
    projections: &[PhysicalProjection],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    for (_, expression) in projections {
        if expression_may_return_set(engine, resolver, expression, schema, params)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(in crate::sql) fn expression_may_return_set(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
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
            if function_may_return_set(
                engine,
                resolver,
                name,
                binding.as_ref(),
                args,
                schema,
                params,
            )? || expressions_may_return_set(engine, resolver, args, schema, params)?
                || expressions_may_return_set(
                    engine,
                    resolver,
                    order_by.iter().map(|order| &order.expr),
                    schema,
                    params,
                )?
            {
                return Ok(true);
            }
            filter.as_deref().map_or(Ok(false), |filter| {
                expression_may_return_set(engine, resolver, filter, schema, params)
            })
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            expressions_may_return_set(engine, resolver, items, schema, params)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => Ok(expression_may_return_set(
            engine, resolver, lhs, schema, params,
        )? || expression_may_return_set(
            engine, resolver, rhs, schema, params,
        )?),
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            expression_may_return_set(engine, resolver, inner, schema, params)
        }
        ScalarExpr::Between { expr, low, high } => {
            Ok(
                expression_may_return_set(engine, resolver, expr, schema, params)?
                    || expression_may_return_set(engine, resolver, low, schema, params)?
                    || expression_may_return_set(engine, resolver, high, schema, params)?,
            )
        }
        ScalarExpr::InList { expr, list, .. } => Ok(expression_may_return_set(
            engine, resolver, expr, schema, params,
        )? || expressions_may_return_set(
            engine, resolver, list, schema, params,
        )?),
        ScalarExpr::WindowCall { args, spec, .. } => {
            if expressions_may_return_set(engine, resolver, args, schema, params)?
                || expressions_may_return_set(engine, resolver, &spec.partition_by, schema, params)?
                || expressions_may_return_set(
                    engine,
                    resolver,
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
                frame_bound_may_return_set(engine, resolver, &frame.start, schema, params)?
                    || frame_bound_may_return_set(engine, resolver, &frame.end, schema, params)?,
            )
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                if expression_may_return_set(engine, resolver, base, schema, params)? {
                    return Ok(true);
                }
            }
            for (condition, result) in when {
                if expression_may_return_set(engine, resolver, condition, schema, params)?
                    || expression_may_return_set(engine, resolver, result, schema, params)?
                {
                    return Ok(true);
                }
            }
            if let Some(branch) = else_branch {
                if expression_may_return_set(engine, resolver, branch, schema, params)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        ScalarExpr::InSubquery { expr, .. } => {
            expression_may_return_set(engine, resolver, expr, schema, params)
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
    resolver: &dyn FunctionTypeResolver,
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    for expression in expressions {
        if expression_may_return_set(engine, resolver, expression, schema, params)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn frame_bound_may_return_set(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
    bound: &ScalarFrameBound,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            expression_may_return_set(engine, resolver, expression, schema, params)
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
    resolver: &dyn FunctionTypeResolver,
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
    message: &str,
) -> Result<(), SQLError> {
    if expressions_may_return_set(engine, resolver, expressions, schema, params)? {
        return Err(set_context_error(message));
    }
    Ok(())
}

fn validate_set_context(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
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
                    resolver,
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
                    resolver,
                    args,
                    schema,
                    params,
                    "set-returning functions are not allowed in COALESCE",
                )?;
            }
            for argument in args {
                validate_set_context(engine, resolver, argument, schema, params)?;
            }
            for order in order_by {
                validate_set_context(engine, resolver, &order.expr, schema, params)?;
            }
            if let Some(filter) = filter {
                validate_set_context(engine, resolver, filter, schema, params)?;
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            reject_set_descendant(
                engine,
                resolver,
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
                resolver,
                descendants,
                schema,
                params,
                "set-returning functions are not allowed in CASE",
            )?;
        }
        ScalarExpr::Not(inner) => {
            reject_set_descendant(
                engine,
                resolver,
                [inner.as_ref()],
                schema,
                params,
                "argument of NOT must not return a set",
            )?;
        }
        ScalarExpr::UnaryMinus(inner) => {
            validate_set_context(engine, resolver, inner, schema, params)?;
        }
        ScalarExpr::And(items) => {
            reject_set_descendant(
                engine,
                resolver,
                items,
                schema,
                params,
                "argument of AND must not return a set",
            )?;
        }
        ScalarExpr::Or(items) => {
            reject_set_descendant(
                engine,
                resolver,
                items,
                schema,
                params,
                "argument of OR must not return a set",
            )?;
        }
        ScalarExpr::InList { expr, list, .. } => {
            validate_set_context(engine, resolver, expr, schema, params)?;
            reject_set_descendant(
                engine,
                resolver,
                list,
                schema,
                params,
                "argument of IN must not return a set",
            )?;
        }
        ScalarExpr::Between { expr, low, high } => {
            reject_set_descendant(
                engine,
                resolver,
                [expr.as_ref(), low.as_ref(), high.as_ref()],
                schema,
                params,
                "argument of AND must not return a set",
            )?;
        }
        ScalarExpr::Array(items) | ScalarExpr::Row(items) => {
            for item in items {
                validate_set_context(engine, resolver, item, schema, params)?;
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            validate_set_context(engine, resolver, lhs, schema, params)?;
            validate_set_context(engine, resolver, rhs, schema, params)?;
        }
        ScalarExpr::IsNull { expr, .. } | ScalarExpr::Cast { expr, .. } => {
            validate_set_context(engine, resolver, expr, schema, params)?;
        }
        ScalarExpr::InSubquery { expr, .. } => {
            reject_set_descendant(
                engine,
                resolver,
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
    resolver: &dyn FunctionTypeResolver,
    projections: &[ProjectionPlan],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for projection in projections {
        validate_set_context(engine, resolver, &projection.expr, schema, params)?;
    }
    Ok(())
}

pub(in crate::sql) fn validate_query_set_contexts(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    validate_projection_set_contexts(engine, resolver, &statement.projections, schema, params)?;
    if let Some(locking) = statement.locking.first() {
        for projection in &statement.projections {
            if expression_may_return_set(engine, resolver, &projection.expr, schema, params)? {
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
        validate_set_context(engine, resolver, expression, schema, params)?;
    }
    if let Some(predicate) = statement.r#where.as_ref() {
        reject_set_descendant(
            engine,
            resolver,
            [predicate],
            schema,
            params,
            "set-returning functions are not allowed in WHERE",
        )?;
    }
    if let Some(having) = statement.having.as_ref() {
        reject_set_descendant(
            engine,
            resolver,
            [having],
            schema,
            params,
            "set-returning functions are not allowed in HAVING",
        )?;
    }
    if let Some(limit) = statement.limit.as_ref() {
        reject_set_descendant(
            engine,
            resolver,
            [limit],
            schema,
            params,
            "set-returning functions are not allowed in LIMIT",
        )?;
    }
    if let Some(offset) = statement.offset.as_ref() {
        reject_set_descendant(
            engine,
            resolver,
            [offset],
            schema,
            params,
            "set-returning functions are not allowed in OFFSET",
        )?;
    }
    if let Some(source) = statement.from.as_ref() {
        validate_source_set_contexts(engine, resolver, source, schema, params)?;
    }
    Ok(())
}

fn validate_source_set_contexts(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
    source: &SourcePlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            validate_source_set_contexts(engine, resolver, left, schema, params)?;
            validate_source_set_contexts(engine, resolver, right, schema, params)?;
            if let Some(condition) = on {
                reject_set_descendant(
                    engine,
                    resolver,
                    [condition],
                    schema,
                    params,
                    "set-returning functions are not allowed in JOIN conditions",
                )?;
            }
        }
        SourcePlan::Values { rows, .. } => {
            validate_values_set_contexts(engine, resolver, rows, schema, params)?;
        }
        SourcePlan::Function { args, .. } => {
            reject_set_descendant(
                engine,
                resolver,
                args,
                schema,
                params,
                "set-returning functions must appear at top level of FROM",
            )?;
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                reject_set_descendant(
                    engine,
                    resolver,
                    &function.args,
                    schema,
                    params,
                    "set-returning functions must appear at top level of FROM",
                )?;
            }
        }
        SourcePlan::Subquery { .. } | SourcePlan::Table { .. } => {}
    }
    Ok(())
}

pub(in crate::sql) fn validate_source_set_contexts_before_build(
    engine: &Engine,
    resolver: &dyn FunctionTypeResolver,
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
            validate_source_set_contexts_before_build(engine, resolver, left, params, ctes, outer)?;
            let left_schema =
                super::super::bind_source_plan_schema(engine, left, params, ctes, outer)?;
            let implicit_lateral_function = matches!(
                right.as_ref(),
                SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
            );
            let right_scope = (*lateral || implicit_lateral_function)
                .then(|| overlay_set_validation_scope(&left_schema, outer));
            validate_source_set_contexts_before_build(
                engine,
                resolver,
                right,
                params,
                ctes,
                right_scope.as_ref().or(outer),
            )
        }
        SourcePlan::Values { rows, .. } => {
            let empty = RowSchema::default();
            validate_values_set_contexts(engine, resolver, rows, outer.unwrap_or(&empty), params)
        }
        SourcePlan::Function { args, .. } => {
            let empty = RowSchema::default();
            reject_set_descendant(
                engine,
                resolver,
                args,
                outer.unwrap_or(&empty),
                params,
                "set-returning functions must appear at top level of FROM",
            )
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            let empty = RowSchema::default();
            for function in functions {
                reject_set_descendant(
                    engine,
                    resolver,
                    &function.args,
                    outer.unwrap_or(&empty),
                    params,
                    "set-returning functions must appear at top level of FROM",
                )?;
            }
            Ok(())
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
    resolver: &dyn FunctionTypeResolver,
    rows: &[Vec<ScalarExpr>],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    reject_set_descendant(
        engine,
        resolver,
        rows.iter().flatten(),
        schema,
        params,
        "set-returning functions are not allowed in VALUES",
    )
}
