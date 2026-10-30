//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initialize the surviving builtin calls before an executable query starts producing rows.

use crate::{
    catalog::security::builtin_routines::execution::BuiltinRoutinePermissions,
    query::{binding::bind_source_plan_schema, sources::SourceContext, CteScope},
    RowSchema,
};
use uqa_sql::{
    catalog::security::builtin_routines::BuiltinRoutineExecution,
    plan::{QueryPlan, RelationalPlan, SourcePlan},
    SQLError, SQLParam, ScalarExpr,
};

mod inputs;

pub fn query<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    let permissions = BuiltinRoutinePermissions::capture(&context.catalog);
    if permissions.unrestricted() {
        return Ok(());
    }
    initialize(
        context,
        &permissions,
        plan,
        params,
        ctes,
        ctes.row_lock_outer_row().map(|row| &row.schema),
    )
}

pub(crate) fn initialize<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    permissions: &BuiltinRoutinePermissions,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let mut scope = ctes.clone();
    let mut lookup = scope.enter_query_plan_scope(plan)?;
    let retained = context.ctes.queries.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.ctes.runtime.cancellation,
    );
    let ordered = uqa_sql::semantics::order_cte_plans_with_control(&plan.ctes, control)?;
    for cte in ordered.iter() {
        lookup.insert_deferred(uqa_sql::plan::RetainedCtePlan::capture(cte, &control)?);
    }
    let mut visible = lookup.enter_visible_ctes(plan.ctes.iter().map(|cte| cte.name.as_str()));
    let scope = &mut *visible;
    let scheduled =
        crate::query::cte::strategy::schedule_plan_ctes(context.ctes, context.volatility, plan)?;
    for scheduled in scheduled.iter() {
        if !scheduled.deferred {
            if let Some(query) = scheduled.plan.body.query() {
                let definition = scope
                    .definition_scope_for_name(&scheduled.plan.name)
                    .expect("declared CTE scope");
                initialize(context, permissions, query, params, &definition, outer)?;
            }
        }
    }
    initialize_root(context, permissions, &plan.root, params, scope, outer)
}

fn initialize_root<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    permissions: &BuiltinRoutinePermissions,
    root: &RelationalPlan,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    match root {
        RelationalPlan::QueryBlock(block) => {
            let schema = block.from.as_ref().map_or_else(
                || Ok(RowSchema::default()),
                |source| {
                    bind_source_plan_schema(context.ctes.routines, source, params, scope, outer)
                },
            )?;
            let schema = uqa_sql::binding::overlay_outer_schema(&schema, outer);
            let arena_resources = context.ctes.production_resources()?;
            scope.scalar_subqueries = uqa_sql::plan::RetainedQueryArena::capture(
                &block.subqueries,
                &arena_resources.control(),
            )?;
            if let Some(from) = &block.from {
                super::ensure_select_privileges_for_query_block(block, from, scope)?;
            }
            generated_calls(context, permissions, block, params, scope)?;
            let mut initialize_expression = |expression: &ScalarExpr| {
                expression_calls(
                    context,
                    permissions,
                    expression,
                    &schema,
                    &block.subqueries,
                    params,
                    scope,
                )
            };
            if !context.planning.block_input_is_empty(block) {
                if let Some(from) = &block.from {
                    inputs::visit(context.planning, from, &mut initialize_expression)?;
                }
            }
            for expression in block.query_expressions() {
                initialize_expression(expression)?;
            }
            if let Some(from) = block
                .from
                .as_ref()
                .filter(|_| !context.planning.block_input_is_empty(block))
            {
                source_calls(context, permissions, from, params, scope, Some(&schema))?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            initialize(context, permissions, left, params, scope, outer)?;
            initialize(context, permissions, right, params, scope, outer)?;
            for expression in order_by
                .iter()
                .map(|order| &order.expr)
                .chain(limit.as_deref())
                .chain(offset.as_deref())
            {
                expression_calls(
                    context,
                    permissions,
                    expression,
                    &RowSchema::default(),
                    subqueries,
                    params,
                    scope,
                )?;
            }
        }
        RelationalPlan::Values { rows, subqueries } => {
            for expression in rows.iter().flatten() {
                expression_calls(
                    context,
                    permissions,
                    expression,
                    &RowSchema::default(),
                    subqueries,
                    params,
                    scope,
                )?;
            }
        }
    }
    Ok(())
}

pub(crate) fn expression_calls<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    permissions: &BuiltinRoutinePermissions,
    expression: &ScalarExpr,
    schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    uqa_sql::catalog::security::builtin_routines::initialization::visit(expression, &mut |part| {
        match part {
            ScalarExpr::Func {
                binding: Some(binding),
                ..
            } => permissions.require_execute(binding)?,
            ScalarExpr::Func {
                name,
                args,
                binding: None,
                ..
            }
            | ScalarExpr::WindowCall { name, args, .. } => {
                let resolver = context.relational.expression_scope(ctes.clone());
                let arguments = args
                    .iter()
                    .filter(|arg| !matches!(arg, ScalarExpr::Star))
                    .map(|arg| {
                        uqa_sql::scalar_type_with_resolver(arg, schema, params, resolver.as_ref())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                permissions.require_analyzed_set_call(
                    name,
                    &arguments,
                    matches!(part, ScalarExpr::WindowCall { .. }),
                )?;
            }
            ScalarExpr::ScalarSubquery(index)
            | ScalarExpr::Exists {
                subquery: index, ..
            }
            | ScalarExpr::InSubquery {
                subquery: index, ..
            } => {
                let plan = subqueries.get(*index).ok_or_else(|| {
                    SQLError::Internal("builtin privilege subquery slot is missing".into())
                })?;
                initialize(context, permissions, plan, params, ctes, Some(schema))?;
            }
            _ => {}
        }
        Ok(())
    })
}

pub(crate) fn source_calls<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    permissions: &BuiltinRoutinePermissions,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    if context.planning.source_is_empty(source) {
        return Ok(());
    }
    match source {
        SourcePlan::Table {
            name,
            executable_query: Some(query),
            ..
        } => {
            let catalog = ctes.catalog_read_view()?;
            let resolution = ctes.relation_name_resolution()?;
            let view = if ctes.is_visible_cte(name) {
                None
            } else {
                catalog.view_resolved(&resolution, name)?.cloned()
            };
            let mut scope = ctes.definition_scope(name).unwrap_or_else(|| ctes.clone());
            if let Some(view) = view {
                let subject = if view.security_invoker() {
                    scope.privilege_subject()?.clone()
                } else {
                    catalog.view_owner(&view)?
                };
                let scope = scope.enter_privilege_subject(subject);
                initialize(context, permissions, query, params, &scope, outer)?;
            } else {
                initialize(context, permissions, query, params, &scope, outer)?;
            }
        }
        SourcePlan::Subquery { body, .. } => {
            initialize(context, permissions, body, params, ctes, outer)?;
        }
        SourcePlan::Join { left, right, .. } => {
            if !context.planning.join_input_is_empty(source, false) {
                source_calls(context, permissions, left, params, ctes, outer)?;
            }
            if !context.planning.join_input_is_empty(source, true) {
                source_calls(context, permissions, right, params, ctes, outer)?;
            }
        }
        SourcePlan::Function {
            binding: Some(binding),
            ..
        } => permissions.require_execute(binding)?,
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                if let Some(binding) = &function.binding {
                    permissions.require_execute(binding)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn generated_calls<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    permissions: &BuiltinRoutinePermissions,
    block: &uqa_sql::plan::QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    let columns = super::with_scope(ctes, |scope| {
        uqa_sql::semantics::privileges::columns::required(block, scope)
    })?;
    generated_relation_calls(context, permissions, columns, params, ctes)
}

pub(crate) fn generated_relation_calls<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    permissions: &BuiltinRoutinePermissions,
    columns: uqa_sql::semantics::privileges::columns::ColumnDependencies,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    let catalog = ctes.catalog_read_view()?;
    let mut resolution = ctes.relation_name_resolution()?;
    resolution.set_lookup_mode(uqa_sql::catalog::resolution::RelationLookupMode::Bound);
    for (name, required) in columns {
        let Some(table) = catalog.table(&resolution, &name)? else {
            continue;
        };
        let schema = RowSchema::with_types(
            table.columns.iter().map(|c| c.name.clone()).collect(),
            table.columns.iter().map(|c| Some(c.ty.clone())).collect(),
        );
        for column in table.columns.iter().filter(|c| required.contains(&c.name)) {
            let Some(generated) = column
                .generated
                .as_ref()
                .filter(|g| g.kind == uqa_sql::ast::GeneratedColumnKind::Virtual)
            else {
                continue;
            };
            let expression = crate::query::generated::prepare_generated_column(&schema, generated)?;
            expression_calls(
                context,
                permissions,
                &expression,
                &schema,
                &[],
                params,
                ctes,
            )?;
        }
    }
    Ok(())
}
