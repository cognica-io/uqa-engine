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
mod outer_constants;
mod streaming_sets;
use uqa_sql::{plan::QueryBlockPlan, semantics::projection_columns};

/// Execute a correlated query using the physical outer row and its lexical scope.
pub fn execute_lateral_subquery_output<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    plan: &QueryPlan,
    outer_row: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<QueryOutput, SQLError> {
    execute_lateral_subquery_with_output(
        context,
        plan,
        outer_row,
        params,
        ctes,
        QueryOutputMode::SharedSpill,
    )
}

pub(crate) fn execute_lateral_subquery_with_output<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    plan: &QueryPlan,
    outer_row: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    output_mode: QueryOutputMode<'_>,
) -> Result<QueryOutput, SQLError> {
    execute_lateral_subquery_output_inner(context, plan, outer_row, params, ctes, output_mode)
}

fn execute_lateral_subquery_output_inner<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    plan: &QueryPlan,
    outer_row: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    output_mode: QueryOutputMode<'_>,
) -> Result<QueryOutput, SQLError> {
    let mut scoped_ctes = ctes.clone();
    scoped_ctes.set_row_lock_outer_row(outer_row.clone());
    let mut lookup = scoped_ctes.enter_relation_lookup_mode(plan.relations_bound)?;
    let mut scoped_ctes = lookup.enter_visible_ctes(plan.ctes.iter().map(|cte| cte.name.as_str()));
    // Keep folded occurrences selected during initialization instead of materializing their original, unpruned definitions for each outer row.
    let scheduled = crate::query::cte::strategy::schedule_plan_ctes(context.volatility, plan)?;
    crate::query::cte::install_scheduled_ctes(
        context.ctes,
        scheduled,
        params,
        &mut scoped_ctes,
        &std::collections::BTreeMap::new(),
    )?;
    execute_lateral_relational_root_output(
        context,
        plan,
        outer_row,
        params,
        &mut scoped_ctes,
        output_mode,
    )
}

fn execute_lateral_relational_root_output<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    plan: &QueryPlan,
    outer_row: &crate::OwnedPhysicalRow,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    output_mode: QueryOutputMode<'_>,
) -> Result<QueryOutput, SQLError> {
    if let Some(output) =
        streaming_sets::try_stream_union(context, plan, outer_row, params, ctes, &output_mode)?
    {
        return Ok(output);
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            execute_lateral_query_block_output(context, block, outer_row, params, ctes, output_mode)
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
                QueryOutputMode::SharedSpill,
            )?;
            let columns = lhs.columns.clone();
            let lhs = query_output_shared(lhs, "lateral set left")?;
            let rhs = execute_lateral_subquery_output_inner(
                context,
                right,
                outer_row,
                params,
                &scoped_ctes,
                QueryOutputMode::SharedSpill,
            )?;
            let rhs = query_output_shared(rhs, "lateral set right")?;
            let order_plan =
                (!order_by.is_empty() || limit.is_some() || offset.is_some()).then(|| {
                    QueryBlockPlan {
                        privilege_columns: std::collections::BTreeSet::default(),
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
                    }
                });
            let execution = crate::query::relational::sets::SetSpillExecution::new(
                *kind,
                *all,
                columns,
                lhs,
                rhs,
                order_plan.as_ref(),
                output_mode,
            );
            crate::query::relational::sets::combine_set_spills_with_order_output(
                context.relational,
                execution,
                params,
                &scoped_ctes,
            )
        }
        RelationalPlan::Values { rows, subqueries } => {
            crate::query::relational::values::execute_plan_values_output(
                context.relational,
                rows,
                subqueries,
                params,
                ctes,
                output_mode,
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
    output_mode: QueryOutputMode<'_>,
) -> Result<QueryOutput, SQLError> {
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
    let (mut stmt, own_schema) = crate::query::binding::bind_query_block_for_execution(
        context.ctes.routines,
        stmt,
        params,
        &scoped_ctes,
        Some(&outer_row.schema),
        false,
    )?;
    // The operator of a lateral block also holds its outer row, whose columns must not take a GROUP BY name before an output column, so the names are bound against the block's own columns first.
    if matches!(stmt.compute, ComputePlan::Aggregate) {
        uqa_sql::semantics::grouping_sets::bind_grouping_names(
            context.ctes.routines,
            stmt.to_mut(),
            &own_schema,
            None,
            params,
        )?;
    }
    let stmt = stmt.as_ref();
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
    let (operator, residual) = build_lateral_query_source(
        context,
        stmt,
        &own_schema,
        outer_row,
        params,
        &mut scoped_ctes,
    )?;
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
        Some(&outer_row.schema),
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
        residual,
        stmt,
        stmt,
        params,
        &scoped_ctes,
        columns,
        output_mode,
    )
}

/// Build the block's sources with the conjuncts this invocation can place in them, and return the remaining filter over the overlaid rows.
fn build_lateral_query_source<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    stmt: &QueryBlockPlan,
    own_schema: &crate::RowSchema,
    outer_row: &crate::OwnedPhysicalRow,
    params: &'a [SQLParam],
    scoped_ctes: &mut CteScope<S>,
) -> Result<
    (
        Box<dyn crate::PhysicalOperator + 'a>,
        Option<uqa_sql::ScalarExpr>,
    ),
    SQLError,
> {
    let placement = outer_constants::placement(context, stmt, own_schema, outer_row, scoped_ctes)?;
    let filters = placement.filters.as_ref();
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
            build_join_operator_with_ctes(context, from, params, &mut source_scope, None, filters)?
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
    Ok((operator, placement.residual))
}
