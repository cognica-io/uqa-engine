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
    for cte in &plan.ctes {
        scope.insert_deferred(cte.clone());
    }
    let mut lookup = scope.enter_relation_lookup_mode(plan.relations_bound)?;
    let mut visible = lookup.enter_visible_ctes(plan.ctes.iter().map(|cte| cte.name.as_str()));
    let scope = &mut *visible;
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            let schema = block.from.as_ref().map_or_else(
                || Ok(RowSchema::default()),
                |source| {
                    bind_source_plan_schema(context.ctes.routines, source, params, scope, outer)
                },
            )?;
            let schema = uqa_sql::binding::overlay_outer_schema(&schema, outer);
            scope.scalar_subqueries.clone_from(&block.subqueries);
            if let Some(from) = &block.from {
                super::ensure_select_privileges_for_query_block(block, from, scope)?;
            }
            generated_calls(context, permissions, block, params, scope)?;
            for expression in block.expressions() {
                expression_calls(
                    context,
                    permissions,
                    expression,
                    &schema,
                    &block.subqueries,
                    params,
                    scope,
                )?;
            }
            if let Some(from) = &block.from {
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
    match source {
        SourcePlan::Subquery { body, .. } => {
            initialize(context, permissions, body, params, ctes, outer)?;
        }
        SourcePlan::Join { left, right, .. } => {
            source_calls(context, permissions, left, params, ctes, outer)?;
            source_calls(context, permissions, right, params, ctes, outer)?;
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
    columns: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
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
