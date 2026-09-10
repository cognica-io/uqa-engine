//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Pull-based derived-table projection assembly.

/// Build a single-consumer derived-table projection as a pull pipeline. Blocking operators inside the query block retain their own bounded state, but a second `SharedSpill` boundary would eagerly exhaust that pipeline before the parent can apply demand such as `LIMIT`.
use super::{
    bind_source_plan_schema_for_execution, build_join_operator_with_ctes, AccessPathPlan,
    ComputePlan, CteScope, RelationalPlan, SQLError, SQLParam, SourceContext,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
pub(super) fn try_build_streaming_subquery_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    body: &uqa_sql::plan::QueryPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
) -> Result<Option<Box<dyn crate::PhysicalOperator + 'a>>, SQLError> {
    let mut relation_lookup = ctes.enter_relation_lookup_mode(body.relations_bound)?;
    let ctes = &mut *relation_lookup;
    if !body.ctes.is_empty()
        || (!ctes.streams_command_progress()
            && uqa_sql::semantics::volatility::query_contains_volatile_function(
                context.volatility,
                body,
            )?)
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
            .is_some_and(uqa_sql::semantics::contains_retrieval)
        || block.from.is_none()
        || block.distinct
        || !block.distinct_on.is_empty()
    {
        return Ok(None);
    }

    // The block's scalar subqueries live in their own arena for the whole pull pipeline: the evaluators built below snapshot this scope, so a derived table with subqueries still streams and an outer LIMIT keeps its inner locking demand-driven.
    let mut ctes = ctes.enter_scalar_subqueries(&block.subqueries);
    let ctes: &mut CteScope<S> = &mut ctes;
    let source_schema = bind_source_plan_schema_for_execution(
        context.ctes.routines,
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
    crate::query::privileges::ensure_select_privileges_for_query_block(block, from, ctes)?;
    let projections = crate::query::projection::physical_projections(&block.projections);
    let type_resolver = context.relational.expression_scope(ctes.clone());
    if uqa_sql::semantics::sets::validation::projections_may_return_set(
        context.relational.catalog,
        type_resolver.as_ref(),
        &projections,
        &source_schema,
        params,
    )? {
        return Ok(None);
    }
    let (_, order_output) =
        crate::query::ordering::order_projection(&block.projections, &source_schema)?;
    for order in &block.order_by {
        let expression =
            crate::query::ordering::resolve_order_expression(&order.expr, &order_output)?;
        if uqa_sql::semantics::sets::validation::expression_may_return_set(
            context.relational.catalog,
            type_resolver.as_ref(),
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
        let column_prune = context.planning.column_prune(block, from, ctes)?;
        let qualifier_filters = context.planning.qualifier_filters(block, from, ctes)?;
        let source_row_locks = crate::query::locking::resolve_row_locks(
            context.locking,
            from,
            &block.locking,
            block.r#where.as_ref(),
            params,
            ctes,
        )?;
        let operator = {
            let mut scoped_ctes = ctes.enter_source_row_locks(source_row_locks);
            build_join_operator_with_ctes(
                context,
                from,
                params,
                &mut scoped_ctes,
                column_prune.as_ref(),
                qualifier_filters.as_ref(),
            )?
        };
        let residual =
            context
                .planning
                .residual_filter(block, from, qualifier_filters.as_ref(), ctes)?;
        let (mut operator, resjunk) = crate::query::relational::build_relational_operator(
            context.relational,
            operator,
            residual,
            block,
            params,
            ctes,
            context.relational.runtime,
        )?;
        if !resjunk.is_empty() {
            operator = Box::new(crate::ColumnSelection::dropping_internal_attributes(
                operator,
                &resjunk.columns(),
            ));
        }
        Ok(Some(operator))
    })();
    ctes.lock_identities = previous_lock_identities;
    result
}
