//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Build a retained child using the same `UnifiedPlan` and physical operators as ordinary queries.

use super::context::QueryContext;
use crate::physical::construction::OperatorBuild;
use crate::query::{
    binding::bind_query_plan_schema,
    cte::{install_statement_ctes, strategy::schedule_plan_ctes},
    sources::try_build_streaming_subquery_operator,
    CteScope,
};
use crate::{ColumnSelection, PhysicalOperator};
use uqa_sql::{
    plan::{AccessPathPlan, ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan},
    semantics::primary_query_cte_references_with_control,
    SQLError, SQLParam,
};

pub fn try_build_retained_query_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'a, S>,
    plan: &QueryPlan,
    params: &'a [SQLParam],
    scope: &CteScope<S>,
) -> Result<Option<Box<dyn PhysicalOperator + 'a>>, SQLError> {
    if !supports_retained_root(plan) {
        return Ok(None);
    }
    let mut scope = scope.clone();
    let mut scope = scope.enter_query_plan_scope(plan)?;
    let prepared =
        crate::query::sources::prepared_queries::prepare(&context.source, plan, params, &scope)?;
    let plan = prepared.as_ref();
    crate::query::privileges::routine_calls::query(&context.source, plan, params, &scope)?;
    scope.retain_cte_producers(!plan.ctes.is_empty(), |scope| {
        build_retained_root(context, plan, params, scope)
    })
}

fn build_retained_root<'a, S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'a, S>,
    plan: &QueryPlan,
    params: &'a [SQLParam],
    scope: &mut CteScope<S>,
) -> Result<Option<Box<dyn PhysicalOperator + 'a>>, SQLError> {
    let mut scope = scope.enter_visible_ctes(plan.ctes.iter().map(|cte| cte.name.as_str()));
    if !plan.ctes.is_empty() {
        let scheduled = schedule_plan_ctes(context.source.ctes, context.source.volatility, plan)?;
        let filters = context.cte_filters.output_filters(plan, &scope)?;
        install_statement_ctes(
            context.source.ctes,
            &plan.ctes,
            &scheduled,
            |control| primary_query_cte_references_with_control(plan, control),
            params,
            &mut scope,
            &filters,
        )?;
    }
    let mut root = plan.clone();
    root.ctes.clear();
    match &root.root {
        RelationalPlan::QueryBlock(_) => {
            try_build_streaming_subquery_operator(&context.source, &root, params, &mut scope)
        }
        RelationalPlan::Values { rows, subqueries } => {
            let scope = scope.enter_scalar_subqueries(subqueries);
            let outer = scope.row_lock_outer_row().cloned();
            let schema = crate::query::binding::values_schema_in_scope(
                context.source.ctes.routines,
                rows,
                subqueries,
                outer.as_ref().map(|row| &row.schema),
                params,
                &scope,
            )?;
            let operator = crate::query::sources::values_source::build_values_scan(
                context.source.types,
                context.source.relational.expression_scope((*scope).clone()),
                params,
                rows,
                schema.column_types(),
                outer,
                schema.clone(),
            )?;
            Ok(Some(operator))
        }
        RelationalPlan::SetOp { .. } => build_set_operator(context, &root, params, &scope),
    }
}

fn build_set_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'a, S>,
    plan: &QueryPlan,
    params: &'a [SQLParam],
    scope: &CteScope<S>,
) -> Result<Option<Box<dyn PhysicalOperator + 'a>>, SQLError> {
    let RelationalPlan::SetOp {
        kind,
        all,
        left,
        right,
        order_by,
        limit,
        with_ties,
        offset,
        subqueries,
    } = &plan.root
    else {
        unreachable!()
    };
    let schema = bind_query_plan_schema(
        context.source.ctes.routines,
        plan,
        params,
        scope,
        scope.row_lock_outer_row().map(|row| &row.schema),
    )?;
    let Some(left_op) = try_build_retained_query_operator(context, left, params, scope)? else {
        return Ok(None);
    };
    let (mut left_op, right_op) = OperatorBuild::run(left_op, |_| {
        try_build_retained_query_operator(context, right, params, scope)
    })?;
    let Some(right_op) = right_op else {
        left_op
            .close()
            .map_err(crate::physical::physical_exec_error)?;
        return Ok(None);
    };
    let (left_op, right_op, work_mem) =
        OperatorBuild::pair(left_op, right_op, |left_op, right_op| {
            left_op.attach(|input| project_retained_output(context, input, left, params, scope))?;
            right_op
                .attach(|input| project_retained_output(context, input, right, params, scope))?;
            context.source.relational.runtime.work_mem_bytes()
        })?;
    let operator: Box<dyn PhysicalOperator + 'a> = Box::new(
        crate::ExternalSetOperation::new_with_types_and_evaluator(
            left_op,
            right_op,
            *kind,
            *all,
            schema.column_types().to_vec(),
            work_mem,
            context.source.relational.evaluator(params, scope),
        )
        .map_err(crate::physical::physical_exec_error)?,
    );
    if order_by.is_empty() && limit.is_none() && offset.is_none() {
        return Ok(Some(operator));
    }
    let statement = QueryBlockPlan {
        deferred_outer_rewrites: Vec::new(),
        privilege_columns: uqa_core::memory::OwnedSet::new(),
        projections: Vec::new(),
        from: None,
        r#where: None,
        compute: ComputePlan::Project,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        group_distinct: false,
        having: None,
        order_by: order_by.clone(),
        limit: limit.as_deref().cloned(),
        with_ties: *with_ties,
        offset: offset.as_deref().cloned(),
        distinct: false,
        distinct_on: Vec::new(),
        subqueries: subqueries.clone(),
        access: AccessPathPlan::Row,
        locking: Vec::new(),
        windows: Vec::new(),
    };
    let mut scope = scope.clone();
    let scope = scope.enter_scalar_subqueries(subqueries);
    let output = crate::query::ordering::identity_order_columns(schema.columns());
    crate::query::relational::limit::attach_order_limit(
        operator,
        &statement,
        &output,
        context.source.relational,
        params,
        &scope,
        context.source.relational.runtime,
        context.source.relational.evaluator(params, &scope),
        None,
    )
    .map(Some)
}

fn project_retained_output<'a, S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'a, S>,
    operator: Box<dyn PhysicalOperator + 'a>,
    query: &QueryPlan,
    params: &[SQLParam],
    scope: &CteScope<S>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let (operator, schema) = OperatorBuild::run(operator, |_| {
        bind_query_plan_schema(
            context.source.ctes.routines,
            query,
            params,
            scope,
            scope.row_lock_outer_row().map(|row| &row.schema),
        )
    })?;
    let positions = schema
        .columns()
        .iter()
        .cloned()
        .enumerate()
        .map(|(position, name)| (name, position))
        .collect();
    Ok(Box::new(ColumnSelection::with_positions(
        operator, positions,
    )))
}

// Decide before constructing any producer: a fallback must never repeat a CTE's effects after partially assembling an unsupported root.
fn supports_retained_root(plan: &QueryPlan) -> bool {
    if plan.ctes.iter().any(|cte| cte.body.modifies_data()) {
        return false;
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            !matches!(block.access, AccessPathPlan::Hybrid)
                && !block
                    .r#where
                    .as_ref()
                    .is_some_and(uqa_sql::semantics::contains_retrieval)
        }
        RelationalPlan::SetOp { left, right, .. } => {
            supports_retained_root(left) && supports_retained_root(right)
        }
        RelationalPlan::Values { .. } => true,
    }
}
