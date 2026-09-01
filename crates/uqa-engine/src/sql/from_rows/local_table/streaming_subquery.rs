//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Pull-based derived-table projection assembly.

use super::{
    build_join_operator_with_ctes, query_contains_volatile_function, resolve_row_locks,
    AccessPathPlan, ComputePlan, CteScope, Engine, RelationalPlan, SQLError, SQLParam,
};

/// Build a single-consumer derived-table projection as a pull pipeline. Blocking operators inside the query block retain their own bounded state, but a second `SharedSpill` boundary would eagerly exhaust that pipeline before the parent can apply demand such as `LIMIT`.
#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
pub(super) fn try_build_streaming_subquery_operator<'a>(
    engine: &'a Engine,
    body: &uqa_planner::QueryPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
) -> Result<Option<Box<dyn uqa_execution::PhysicalOperator + 'a>>, SQLError> {
    if !body.ctes.is_empty()
        || (!ctes.streams_command_progress() && query_contains_volatile_function(engine, body)?)
    {
        return Ok(None);
    }
    let RelationalPlan::QueryBlock(block) = &body.root else {
        return Ok(None);
    };
    let mut block = block.clone();
    // A block whose qualification calls a registered retrieval function (text_match, knn_match, graph_traverse, rpq, ...) executes it through the operator-tree bridge of the single-table executor; the residual scalar filter of a streamed block cannot evaluate such calls. Plain comparisons keep streaming so an outer LIMIT still bounds locking demand inside the derived table.
    if !matches!(block.compute, ComputePlan::Project)
        || matches!(block.access, AccessPathPlan::Hybrid)
        || block
            .r#where
            .as_ref()
            .is_some_and(uqa_planner::optimizer::contains_retrieval)
        || block.from.is_none()
        || block.distinct
        || !block.distinct_on.is_empty()
    {
        return Ok(None);
    }

    // The block's scalar subqueries live in their own arena for the whole pull pipeline: the evaluators built below snapshot this scope, so a derived table with subqueries still streams and an outer LIMIT keeps its inner locking demand-driven.
    let mut ctes = ctes.enter_scalar_subqueries(&block.subqueries);
    let ctes: &mut CteScope = &mut ctes;
    let source_schema = crate::sql::select::bind_source_plan_schema_for_execution(
        engine,
        block
            .from
            .as_mut()
            .expect("derived-table FROM checked above"),
        params,
        ctes,
        None,
    )?;
    let block = &*block;
    let from = block
        .from
        .as_ref()
        .expect("derived-table FROM checked above");
    let projections = crate::sql::select::physical_projections(&block.projections);
    let type_resolver = crate::sql::select::ScopedEngineHook::new(engine, ctes);
    if crate::sql::select::projections_may_return_set(
        engine,
        &type_resolver,
        &projections,
        &source_schema,
        params,
    )? {
        return Ok(None);
    }
    let (_, order_output) =
        crate::sql::select::order_projection(&block.projections, &source_schema)?;
    for order in &block.order_by {
        let expression = crate::sql::select::resolve_order_expression(&order.expr, &order_output)?;
        if crate::sql::select::expression_may_return_set(
            engine,
            &type_resolver,
            &expression,
            &source_schema,
            params,
        )? {
            return Ok(None);
        }
    }

    let emit_lock_identities = ctes.lock_identities.emit || !block.locking.is_empty();
    let previous_lock_identities = ctes.lock_identities;
    ctes.lock_identities.emit = emit_lock_identities;
    ctes.lock_identities.retain_after_lock = previous_lock_identities.emit;
    let result = (|| {
        let column_prune = crate::sql::select::column_prune_for_stmt(engine, block, from, ctes)?;
        let qualifier_filters =
            crate::sql::select::qualifier_filters_for_stmt(engine, block, from, ctes)?;
        let source_row_locks = resolve_row_locks(
            engine,
            from,
            &block.locking,
            block.r#where.as_ref(),
            params,
            ctes,
        )?;
        let operator = {
            let mut scoped_ctes = ctes.enter_source_row_locks(source_row_locks);
            build_join_operator_with_ctes(
                engine,
                from,
                params,
                &mut scoped_ctes,
                column_prune.as_ref(),
                qualifier_filters.as_ref(),
            )?
        };
        let residual = crate::sql::select::final_filter_after_qualifier_pushdown(
            engine,
            block,
            from,
            qualifier_filters.as_ref(),
            ctes,
        )?;
        let (mut operator, resjunk) = crate::sql::select::build_relational_operator(
            engine,
            operator,
            residual,
            block,
            params,
            ctes,
            engine.query_runtime_view(),
        )?;
        if !resjunk.is_empty() {
            operator = Box::new(
                uqa_execution::ColumnSelection::dropping_internal_attributes(
                    operator,
                    &resjunk.columns(),
                ),
            );
        }
        Ok(Some(operator))
    })();
    ctes.lock_identities = previous_lock_identities;
    result
}
