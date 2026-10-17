//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-returning expression detection and SQL context validation.

use crate::ast::FunctionBinding;
use crate::plan::{ProjectionPlan, QueryBlockPlan, SourcePlan};
use crate::{FunctionTypeResolver, RowSchema, ScalarExpr, ScalarFrameBound};
use crate::{SQLError, SQLParam};

use super::placement::{
    conditional_set_error, row_comparison_set_error, set_argument_error, top_level_from,
    SetPlacement,
};
use super::placement_scope::placing_set_calls;
use super::{PhysicalProjection, SetFunctionCatalog};
use crate::binding::context::BindingContext;
use crate::semantics::aggregates::is_aggregate;

pub fn builtin_returns_set(name: &str) -> bool {
    matches!(
        name,
        "generate_series"
            | "pg_listening_channels"
            | "unnest"
            | "regexp_matches"
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
            | "aclexplode"
    )
}

pub fn function_may_return_set(
    engine: &dyn crate::routines::RoutineResolution,
    resolver: &dyn FunctionTypeResolver,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    if binding.and_then(|binding| binding.dispatch).is_some() {
        return Ok(false);
    }
    if binding.is_some_and(FunctionBinding::is_polymorphic_builtin_syntax) {
        return Ok(false);
    }
    if binding.is_some_and(|binding| !binding.builtin) {
        let (argument_names, argument_types, explicit_variadic) =
            crate::function_call_argument_signature(args, schema, params, Some(resolver))?;
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
    let builtin = crate::semantics::builtin_function_dispatch_name(&identity);
    let builtin_returns_set =
        builtin_returns_set(&builtin) || engine.has_registered_table_function(&identity);
    // An unbound call of a set-returning built-in's name returns a set unless overload resolution selects a SQL routine of that name.
    if builtin_returns_set
        && (binding.is_some()
            || engine
                .lookup_visible_sql_functions_for_analysis(name)?
                .is_none())
    {
        return Ok(true);
    }
    let (argument_names, argument_types, explicit_variadic) =
        crate::function_call_argument_signature(args, schema, params, Some(resolver))?;
    let Some(overloads) = engine.lookup_visible_sql_functions_for_analysis(name)? else {
        return Ok(false);
    };
    if !builtin_returns_set && !crate::is_fixed_builtin(name) {
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
    if let Some(resolved) = crate::resolve_fixed_builtin_call(
        name,
        binding,
        &argument_names,
        &argument_types,
        explicit_variadic,
        Some(resolver),
    )? {
        if resolved.selected.binding.builtin {
            return Ok(builtin_returns_set);
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
        Ok(Some(function)) => Ok(function.def.returns_set()),
        Ok(None) => Ok(builtin_returns_set),
        // No SQL routine of a set-returning built-in's name accepts the arguments, so the built-in does.
        Err(error) if builtin_returns_set && error.sqlstate() == Some("42883") => Ok(true),
        Err(error) if binding.is_none() && error.sqlstate() == Some("42883") => {
            match crate::type_resolution::builtin_function_type_with_resolver(
                &builtin,
                args,
                &[],
                schema,
                params,
                resolver,
            ) {
                Ok(Some(_)) => Ok(false),
                Ok(None) | Err(_) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

pub fn resolve_set_function_binding(
    engine: &dyn SetFunctionCatalog,
    resolver: &dyn FunctionTypeResolver,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<FunctionBinding>, SQLError> {
    let (argument_names, argument_types, explicit_variadic) =
        crate::function_call_argument_signature(args, schema, params, Some(resolver))?;
    let unbound_without_routines = binding.is_none()
        && engine
            .lookup_visible_sql_functions_for_analysis(name)?
            .is_none();
    if let Some(resolved) = crate::resolve_fixed_builtin_call(
        name,
        binding,
        &argument_names,
        &argument_types,
        explicit_variadic,
        Some(resolver),
    )? {
        // A call keeps its recorded built-in binding. A stored call analyzed before its built-in had catalog signatures takes the selected declaration, which EXECUTE authorization identifies.
        return Ok((binding.is_none() || !resolved.selected.binding.builtin)
            .then_some(resolved.selected.binding));
    }
    if unbound_without_routines {
        return Ok(None);
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

pub fn projections_may_return_set(
    engine: &dyn SetFunctionCatalog,
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

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub fn expression_may_return_set(
    engine: &dyn crate::routines::RoutineResolution,
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
        | ScalarExpr::CompositeRow { items, .. }
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
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => {
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
        // A window call returns one value per row: its arguments, FILTER and frame admit no set-returning call, and its partition and ordering keys expand the rows below the window, as `expression_returns_set` treats `WindowFunc`.
        ScalarExpr::WindowCall { .. } => Ok(false),
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
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => Ok(false),
    }
}

fn expressions_may_return_set<'a>(
    engine: &dyn crate::routines::RoutineResolution,
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

/// Reject `expressions` when any contains a set-returning call outside its subqueries.
fn reject_set_descendant<'a>(
    engine: &dyn SetFunctionCatalog,
    resolver: &dyn FunctionTypeResolver,
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
    error: impl FnOnce() -> SQLError,
) -> Result<(), SQLError> {
    if expressions_may_return_set(engine, resolver, expressions, schema, params)? {
        return Err(error());
    }
    Ok(())
}

/// Reject a set-returning call in a clause whose expression kind admits none.
fn reject_set_placement<'a>(
    engine: &dyn SetFunctionCatalog,
    resolver: &dyn FunctionTypeResolver,
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
    placement: SetPlacement,
) -> Result<(), SQLError> {
    if expressions_may_return_set(engine, resolver, expressions, schema, params)? {
        placement.admit(true)?;
    }
    Ok(())
}

/// Check a target-list expression in `PostgreSQL`'s transformation order: each construct's inputs first, then the construct.
#[expect(
    clippy::too_many_lines,
    reason = "one ordered dispatch covers every scalar expression"
)]
fn validate_set_context(
    engine: &dyn SetFunctionCatalog,
    resolver: &dyn FunctionTypeResolver,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let validate = |expression| validate_set_context(engine, resolver, expression, schema, params);
    let returns_set =
        |expression| expression_may_return_set(engine, resolver, expression, schema, params);
    match expression {
        ScalarExpr::Func {
            name,
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                validate(argument)?;
            }
            if let Some(filter) = filter {
                reject_set_placement(
                    engine,
                    resolver,
                    [filter.as_ref()],
                    schema,
                    params,
                    SetPlacement::Filter,
                )?;
            }
            for order in order_by {
                validate(&order.expr)?;
            }
            if is_aggregate(engine, expression) {
                reject_set_descendant(
                    engine,
                    resolver,
                    args.iter().chain(order_by.iter().map(|order| &order.expr)),
                    schema,
                    params,
                    || {
                        conditional_set_error(
                            "aggregate function calls cannot contain set-returning function calls",
                        )
                    },
                )?;
            } else if crate::semantics::builtin_function_dispatch_name(name) == "coalesce" {
                reject_set_descendant(engine, resolver, args, schema, params, || {
                    conditional_set_error("set-returning functions are not allowed in COALESCE")
                })?;
            }
        }
        ScalarExpr::WindowCall {
            args, spec, filter, ..
        } => {
            for argument in args {
                validate(argument)?;
            }
            if let Some(filter) = filter {
                reject_set_placement(
                    engine,
                    resolver,
                    [filter.as_ref()],
                    schema,
                    params,
                    SetPlacement::Filter,
                )?;
            }
            reject_set_descendant(engine, resolver, args, schema, params, || {
                conditional_set_error(
                    "window function calls cannot contain set-returning function calls",
                )
            })?;
            for expression in spec
                .partition_by
                .iter()
                .chain(spec.order_by.iter().map(|order| &order.expr))
            {
                validate(expression)?;
            }
            reject_set_placement(
                engine,
                resolver,
                spec.frame
                    .iter()
                    .flat_map(|frame| [&frame.start, &frame.end])
                    .filter_map(|bound| frame_bound_expression(bound)),
                schema,
                params,
                SetPlacement::WindowFrame,
            )?;
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                validate(base)?;
            }
            for (condition, result) in when {
                validate(condition)?;
                // A searched WHEN is coerced to boolean; a simple CASE compares a placeholder for its base with the WHEN value.
                if returns_set(condition)? {
                    return Err(set_argument_error("CASE/WHEN"));
                }
                validate(result)?;
            }
            if let Some(branch) = else_branch {
                validate(branch)?;
            }
            let descendants = base
                .iter()
                .map(AsRef::as_ref)
                .chain(
                    when.iter()
                        .flat_map(|(condition, result)| [condition, result]),
                )
                .chain(else_branch.iter().map(AsRef::as_ref));
            reject_set_descendant(engine, resolver, descendants, schema, params, || {
                conditional_set_error("set-returning functions are not allowed in CASE")
            })?;
        }
        ScalarExpr::Not(inner) => {
            validate(inner)?;
            if returns_set(inner)? {
                return Err(set_argument_error("NOT"));
            }
        }
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => validate(inner)?,
        ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            let construct = if matches!(expression, ScalarExpr::And(_)) {
                "AND"
            } else {
                "OR"
            };
            for item in items {
                validate(item)?;
                if returns_set(item)? {
                    return Err(set_argument_error(construct));
                }
            }
        }
        ScalarExpr::InList { expr, list, .. } => {
            validate(expr)?;
            for item in list {
                validate(item)?;
            }
            reject_set_descendant(engine, resolver, list, schema, params, || {
                set_argument_error("IN")
            })?;
        }
        ScalarExpr::Between { expr, low, high } => {
            for item in [expr, low, high] {
                validate(item)?;
            }
            reject_set_descendant(
                engine,
                resolver,
                [expr.as_ref(), low.as_ref(), high.as_ref()],
                schema,
                params,
                || set_argument_error("AND"),
            )?;
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. } => {
            for item in items {
                validate(item)?;
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            validate(lhs)?;
            validate(rhs)?;
            if matches!(
                (lhs.as_ref(), rhs.as_ref()),
                (ScalarExpr::Row(_), ScalarExpr::Row(_))
            ) {
                reject_set_descendant(
                    engine,
                    resolver,
                    [lhs.as_ref(), rhs.as_ref()],
                    schema,
                    params,
                    row_comparison_set_error,
                )?;
            }
        }
        ScalarExpr::InSubquery { expr, .. } => {
            validate(expr)?;
            reject_set_descendant(
                engine,
                resolver,
                [expr.as_ref()],
                schema,
                params,
                row_comparison_set_error,
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
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
    Ok(())
}

pub fn validate_projection_set_contexts(
    engine: &dyn SetFunctionCatalog,
    resolver: &dyn FunctionTypeResolver,
    projections: &[ProjectionPlan],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    if !placing_set_calls() {
        return Ok(());
    }
    for projection in projections {
        validate_set_context(engine, resolver, &projection.expr, schema, params)?;
    }
    Ok(())
}

/// Check a query block's clauses in `PostgreSQL`'s transformation order: the target list, WHERE, HAVING, ORDER BY, GROUP BY, DISTINCT ON, OFFSET, LIMIT and FROM, then the row-locking clause. A block of a stored definition is not checked again.
pub fn validate_query_set_contexts(
    engine: &dyn SetFunctionCatalog,
    resolver: &dyn FunctionTypeResolver,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    if !placing_set_calls() {
        return Ok(());
    }
    validate_projection_set_contexts(engine, resolver, &statement.projections, schema, params)?;
    for (clause, placement) in [
        (statement.r#where.as_ref(), SetPlacement::Where),
        (statement.having.as_ref(), SetPlacement::Having),
    ] {
        reject_set_placement(engine, resolver, clause, schema, params, placement)?;
    }
    for expression in statement
        .order_by
        .iter()
        .map(|order| &order.expr)
        .chain(statement.group_by.iter())
        .chain(statement.grouping_sets.iter().flatten())
        .chain(statement.distinct_on.iter())
    {
        validate_set_context(engine, resolver, expression, schema, params)?;
    }
    for (clause, placement) in [
        (statement.offset.as_ref(), SetPlacement::Offset),
        (statement.limit.as_ref(), SetPlacement::Limit),
    ] {
        reject_set_placement(engine, resolver, clause, schema, params, placement)?;
    }
    if let Some(source) = statement.from.as_ref() {
        validate_source_set_contexts(engine, resolver, source, schema, params)?;
    }
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
    Ok(())
}

fn validate_source_set_contexts(
    engine: &dyn SetFunctionCatalog,
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
                reject_set_placement(
                    engine,
                    resolver,
                    [condition],
                    schema,
                    params,
                    SetPlacement::JoinCondition,
                )?;
            }
        }
        SourcePlan::Values { rows, .. } => {
            validate_values_set_contexts(engine, resolver, rows, schema, params)?;
        }
        SourcePlan::Function { args, .. } => {
            reject_set_descendant(engine, resolver, args, schema, params, top_level_from)?;
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                reject_set_descendant(
                    engine,
                    resolver,
                    &function.args,
                    schema,
                    params,
                    top_level_from,
                )?;
            }
        }
        SourcePlan::Subquery { .. } | SourcePlan::Table { .. } => {}
    }
    Ok(())
}

pub fn validate_source_set_contexts_before_build(
    engine: &dyn SetFunctionCatalog,
    resolver: &dyn FunctionTypeResolver,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &BindingContext<'_>,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    if !placing_set_calls() {
        return Ok(());
    }
    match source {
        SourcePlan::Join {
            left,
            right,
            lateral,
            ..
        } => {
            validate_source_set_contexts_before_build(engine, resolver, left, params, ctes, outer)?;
            let left_schema =
                crate::binding::bind_source_plan_schema(engine, left, params, ctes, outer)?;
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
                top_level_from,
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
                    top_level_from,
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

pub fn validate_values_set_contexts(
    engine: &dyn SetFunctionCatalog,
    resolver: &dyn FunctionTypeResolver,
    rows: &[Vec<ScalarExpr>],
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    if !placing_set_calls() {
        return Ok(());
    }
    reject_set_placement(
        engine,
        resolver,
        rows.iter().flatten(),
        schema,
        params,
        SetPlacement::Values,
    )
}

#[cfg(test)]
mod tests;
