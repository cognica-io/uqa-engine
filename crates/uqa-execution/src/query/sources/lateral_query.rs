//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Correlated query blocks, set operations, and VALUES in a physical outer scope.

use super::{
    build_join_operator_with_ctes, query_output_shared, AccessPathPlan, ComputePlan, CteScope,
    QueryOutput, QueryOutputMode, QueryPlan, RelationalPlan, SQLError, SQLParam, SourceContext,
};
use crate::query::projection::expand_from_star_columns;
use uqa_sql::{plan::QueryBlockPlan, semantics::projection_columns};

/// Execute a correlated query using the physical outer row and its lexical scope.
pub fn execute_lateral_subquery_output<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    plan: &QueryPlan,
    outer_row: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<QueryOutput, SQLError> {
    execute_lateral_subquery_output_inner(context, plan, outer_row, params, ctes)
}

fn execute_lateral_subquery_output_inner<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    plan: &QueryPlan,
    outer_row: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<QueryOutput, SQLError> {
    let mut scoped_ctes = ctes.clone();
    scoped_ctes.set_row_lock_outer_row(outer_row.clone());
    crate::query::cte::materialize_plan_ctes(context.ctes, &plan.ctes, params, &mut scoped_ctes)?;
    execute_lateral_relational_root_output(context, &plan.root, outer_row, params, &mut scoped_ctes)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
fn execute_lateral_relational_root_output<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    root: &RelationalPlan,
    outer_row: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
) -> Result<QueryOutput, SQLError> {
    match root {
        RelationalPlan::QueryBlock(block) => {
            execute_lateral_query_block_output(context, block, outer_row, params, ctes)
        }
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            limit,
            with_ties,
            offset,
            subqueries,
        } => {
            let scoped_ctes = ctes.enter_scalar_subqueries(subqueries);
            let lhs = execute_lateral_subquery_output_inner(
                context,
                left,
                outer_row,
                params,
                &scoped_ctes,
            )?;
            let columns = lhs.columns.clone();
            let lhs = query_output_shared(lhs, "lateral set left")?;
            let rhs = execute_lateral_subquery_output_inner(
                context,
                right,
                outer_row,
                params,
                &scoped_ctes,
            )?;
            let rhs = query_output_shared(rhs, "lateral set right")?;
            let order_plan =
                (!order_by.is_empty() || limit.is_some() || offset.is_some()).then(|| {
                    QueryBlockPlan {
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
                    }
                });
            let execution = crate::query::relational::sets::SetSpillExecution::new(
                *kind,
                *all,
                columns,
                lhs,
                rhs,
                order_plan.as_ref(),
                QueryOutputMode::SharedSpill,
            );
            crate::query::relational::sets::combine_set_spills_with_order_output(
                context.relational,
                execution,
                params,
                &scoped_ctes,
            )
        }
        RelationalPlan::Values { rows, subqueries } => {
            let columns: Vec<String> = rows
                .first()
                .map(|row| {
                    (0..row.len())
                        .map(|index| format!("column{}", index + 1))
                        .collect()
                })
                .unwrap_or_default();
            let hook = context.relational.expression_scope(ctes.clone());
            let scalar_context =
                crate::scalar::plan::PhysicalEvalContext::from_row_lookup(outer_row, params)
                    .with_function_hook(hook.as_ref())
                    .with_subquery_runner(hook.as_ref())
                    .with_physical_outer_row(&outer_row.schema, &outer_row.row);
            let rows = rows
                .iter()
                .map(|values| {
                    values
                        .iter()
                        .map(|expression| {
                            crate::scalar::plan::eval_physical_scalar(
                                expression,
                                subqueries,
                                &scalar_context,
                            )
                        })
                        .collect::<Result<Vec<_>, SQLError>>()
                        .map(crate::PhysicalRow::from_values)
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            let operator: Box<dyn crate::PhysicalOperator + '_> = Box::new(
                crate::TableScan::from_physical_rows(crate::RowSchema::new(columns.clone()), rows),
            );
            crate::query::collection::collect_query_operator(
                context.relational.runtime,
                columns,
                operator,
                QueryOutputMode::SharedSpill,
            )
        }
    }
}

fn execute_lateral_query_block_output<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    stmt: &QueryBlockPlan,
    outer_row: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
    scoped_ctes: &mut CteScope<S>,
) -> Result<QueryOutput, SQLError> {
    let mut stmt = stmt.clone();
    let inherited_lock_identities = scoped_ctes.lock_identities.emit;
    let mut scoped_ctes = scoped_ctes.enter_scalar_subqueries(&stmt.subqueries);
    scoped_ctes.set_row_lock_outer_row(outer_row.clone());
    let row_identity_barrier = stmt.distinct
        || !stmt.distinct_on.is_empty()
        || matches!(stmt.compute, ComputePlan::Aggregate | ComputePlan::Window);
    scoped_ctes.lock_identities.emit =
        !stmt.locking.is_empty() || (inherited_lock_identities && !row_identity_barrier);
    scoped_ctes.lock_identities.retain_after_lock =
        inherited_lock_identities && !row_identity_barrier;
    if let Some(from) = stmt.from.as_mut() {
        crate::query::binding::bind_source_plan_schema_for_execution(
            context.ctes.routines,
            from,
            params,
            &scoped_ctes,
            Some(&outer_row.schema),
        )?;
    }
    let stmt = &stmt;
    if let Some(from) = stmt.from.as_ref() {
        uqa_sql::semantics::sets::validation::validate_source_set_contexts_before_build(
            context.relational.catalog,
            context
                .relational
                .expression_scope((*scoped_ctes).clone())
                .as_ref(),
            from,
            params,
            &crate::query::binding::binding_context(&scoped_ctes)?,
            Some(&outer_row.schema),
        )?;
    }
    let operator = build_lateral_query_source(context, stmt, outer_row, params, &mut scoped_ctes)?;
    let columns = expand_from_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        operator.row_schema(),
    )?;
    crate::query::binding::validate_query_block_expression_types(
        context.ctes.routines,
        stmt,
        operator.row_schema(),
        params,
        &scoped_ctes,
    )?;
    if let (Some(from), Some(filter)) = (stmt.from.as_ref(), stmt.r#where.as_ref()) {
        uqa_sql::semantics::text_indexes::validate_joined_expr_text_match_fields(
            context.text_indexes,
            from,
            filter,
        )?;
    }
    crate::query::binding::validate_query_block_references(
        context.ctes.routines,
        stmt,
        operator.row_schema(),
        params,
        &scoped_ctes,
    )?;
    uqa_sql::semantics::sets::validation::validate_query_set_contexts(
        context.relational.catalog,
        context
            .relational
            .expression_scope((*scoped_ctes).clone())
            .as_ref(),
        stmt,
        operator.row_schema(),
        params,
    )?;
    crate::query::relational::output::execute_query_block_operator_output(
        context.relational,
        operator,
        stmt.r#where.clone(),
        stmt,
        stmt,
        params,
        &scoped_ctes,
        columns,
        QueryOutputMode::SharedSpill,
    )
}

fn build_lateral_query_source<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    stmt: &QueryBlockPlan,
    outer_row: &crate::OwnedPhysicalRow,
    params: &'a [SQLParam],
    scoped_ctes: &mut CteScope<S>,
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    let operator: Box<dyn crate::PhysicalOperator + 'a> = if let Some(from) = stmt.from.as_ref() {
        let source_row_locks = crate::query::locking::resolve_row_locks(
            context.locking,
            from,
            &stmt.locking,
            stmt.r#where.as_ref(),
            params,
            scoped_ctes,
        )?;
        let child = {
            let mut source_scope = scoped_ctes.enter_source_row_locks(source_row_locks);
            build_join_operator_with_ctes(context, from, params, &mut source_scope, None, None)?
        };
        Box::new(crate::ScopeOverlay::new(child, outer_row.clone()))
    } else {
        let child: Box<dyn crate::PhysicalOperator + '_> =
            Box::new(crate::TableScan::from_physical_rows(
                crate::RowSchema::default(),
                vec![crate::PhysicalRow::default()],
            ));
        Box::new(crate::ScopeOverlay::new(child, outer_row.clone()))
    };
    Ok(operator)
}
