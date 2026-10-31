//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Pull-based derived-table projection assembly.

/// Build a single-consumer derived-table projection as a pull pipeline. Blocking operators inside the query block retain their own bounded state, but a second `SharedSpill` boundary would eagerly exhaust that pipeline before the parent can apply demand such as `LIMIT`.
use super::{
    build_join_operator_with_bound_sources, AccessPathPlan, ComputePlan, CteScope, RelationalPlan,
    SQLError, SQLParam, SourceContext,
};
use crate::query::binding::sources::{
    bind_query_block_with_sources, ensure_select_with_sources, BoundQueryBlock, SourceBindings,
};
use crate::query::row_at_a_time::RowAtATime;

pub(crate) fn try_build_streaming_subquery_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    body: &uqa_sql::plan::QueryPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
) -> Result<Option<Box<dyn crate::PhysicalOperator + 'a>>, SQLError> {
    try_build_streaming_subquery_with_sources(context, body, params, ctes, None)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
pub(super) fn try_build_streaming_subquery_with_sources<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    body: &uqa_sql::plan::QueryPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    bindings: Option<&SourceBindings<'_>>,
) -> Result<Option<Box<dyn crate::PhysicalOperator + 'a>>, SQLError> {
    let resources = context.ctes.production_resources()?;
    let mut relation_lookup =
        ctes.enter_query_plan_scope_with_control(body, &resources.control())?;
    let ctes = &mut *relation_lookup;
    if !body.ctes.is_empty() {
        return Ok(None);
    }
    let RelationalPlan::QueryBlock(block) = &body.root else {
        return Ok(None);
    };
    let rewrite_subqueries = !block.deferred_outer_rewrites.is_empty();
    // A block whose qualification calls a registered retrieval function (text_match, knn_match, graph_traverse, rpq, ...) executes it through the operator-tree bridge of the single-table executor; the residual scalar filter of a streamed block cannot evaluate such calls. Plain comparisons keep streaming so an outer LIMIT still bounds locking demand inside the derived table.
    if matches!(block.access, AccessPathPlan::Hybrid)
        || block
            .r#where
            .as_ref()
            .is_some_and(uqa_sql::semantics::contains_retrieval)
    {
        return Ok(None);
    }

    let scalar_demand = !ctes.streams_command_progress()
        && uqa_sql::semantics::volatility::query_contains_volatile_function(
            context.volatility,
            body,
        )?;

    crate::query::privileges::routine_calls::query(context, body, params, ctes)?;

    // The block's scalar subqueries live in their own arena for the whole pull pipeline: the evaluators built below snapshot this scope, so a derived table with subqueries still streams and an outer LIMIT keeps its inner locking demand-driven.
    let arena_resources = context.ctes.production_resources()?;
    let mut ctes = ctes.enter_query_block_with_control(block, &arena_resources.control())?;
    let ctes: &mut CteScope<S> = &mut ctes;
    let outer_row = ctes
        .row_lock_outer_row()
        .map(|outer| crate::query::binding::bind_outer_rewrite_row(block, outer).into_owned());
    if let Some(outer) = &outer_row {
        ctes.set_row_lock_outer_row(outer.clone());
    }
    let BoundQueryBlock {
        mut block,
        schema: source_schema,
        mut sources,
    } = bind_query_block_with_sources(
        context.ctes.routines,
        block,
        params,
        ctes,
        uqa_sql::binding::ExecutionBindingOptions {
            outer: outer_row.as_ref().map(|row| &row.schema),
            defer_distinct_limit: uqa_sql::semantics::should_defer_distinct_limit(block),
        },
        bindings,
        &resources.control(),
    )?;
    // GROUP BY output names precede enclosing columns. Bind them against this block's own input before adding the outer row, as in the lateral executor.
    if outer_row.is_some() && matches!(block.compute, ComputePlan::Aggregate) {
        block = block.bind_grouping_names(
            context.ctes.routines,
            &source_schema,
            None,
            params,
            &resources.control(),
        )?;
    }
    let source_schema = uqa_sql::binding::with_query_table_pseudo_columns_with_control(
        &source_schema,
        &resources.control(),
    )?;
    let source_schema = uqa_sql::binding::overlay_outer_schema_with_control(
        &source_schema,
        outer_row.as_ref().map(|row| &row.schema),
        &resources.control(),
    )?;
    let original = match &body.root {
        RelationalPlan::QueryBlock(original) => original,
        _ => unreachable!(),
    };
    let block_owner = block;
    let block = block_owner.as_ref();
    let mut bound_scope;
    let ctes = if rewrite_subqueries {
        let arena_resources = context.ctes.production_resources()?;
        bound_scope = ctes
            .enter_scalar_subqueries_with_control(&block.subqueries, &arena_resources.control())?;
        &mut *bound_scope
    } else {
        ctes
    };
    if let Some(from) = &block.from {
        sources = ensure_select_with_sources(block, from, ctes, sources.as_ref())?;
    }
    crate::query::binding::validate_query_block_expression_types(
        context.ctes.routines,
        block,
        &source_schema,
        params,
        ctes,
        &context.ctes.production_resources()?.control(),
    )?;
    crate::query::binding::validate_query_block_references(
        context.ctes.routines,
        block,
        &source_schema,
        params,
        ctes,
        outer_row.as_ref().map(|row| &row.schema),
        &context.ctes.production_resources()?.control(),
    )?;
    let resolver = context.relational.expression_scope(ctes)?;
    uqa_sql::semantics::sets::validation::validate_query_set_contexts(
        context.relational.catalog,
        resolver.as_ref(),
        block,
        &source_schema,
        params,
    )?;
    if let Some(from) = &block.from {
        uqa_sql::semantics::sets::validation::validate_source_set_contexts_before_build(
            context.relational.catalog,
            resolver.as_ref(),
            from,
            params,
            &crate::query::binding::binding_context(ctes)?,
            outer_row.as_ref().map(|row| &row.schema),
        )?;
    }
    let row_identity_barrier = original.distinct
        || !original.distinct_on.is_empty()
        || matches!(
            original.compute,
            ComputePlan::Aggregate | ComputePlan::Window
        );
    let emit_lock_identities =
        (ctes.lock_identities.emit && !row_identity_barrier) || !block.locking.is_empty();
    let retain_after_lock = ctes.lock_identities.emit && !row_identity_barrier;
    let mut identities =
        ctes.enter_lock_identity_options(crate::query::scope::LockIdentityOptions {
            emit: emit_lock_identities,
            retain_after_lock,
        });
    let ctes: &mut CteScope<S> = &mut identities;
    let (operator, residual) = if let Some(from) = &block.from {
        let column_prune =
            context
                .planning
                .column_prune(block, from, ctes, Some(&source_schema))?;
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
            let mut scoped_ctes = ctes.enter_source_row_locks_with_control(
                source_row_locks,
                &context.ctes.production_resources()?.control(),
            )?;
            build_join_operator_with_bound_sources(
                context,
                from,
                params,
                &mut scoped_ctes,
                column_prune.as_ref(),
                qualifier_filters.as_ref(),
                sources.as_ref(),
            )?
        };
        crate::physical::construction::OperatorBuild::run(operator, |_| {
            context
                .planning
                .residual_filter(block, from, qualifier_filters.as_ref(), ctes)
        })?
    } else {
        (
            Box::new(crate::TableScan::from_physical_rows(
                crate::RowSchema::default(),
                vec![crate::PhysicalRow::default()],
            )) as Box<dyn crate::PhysicalOperator + 'a>,
            block.r#where.clone(),
        )
    };
    let pending = !block.deferred_outer_rewrites.is_empty();
    drop(sources);
    let (operator, (rewritten, rewritten_original)) =
        crate::physical::construction::OperatorBuild::run(operator, |operator| {
            let rewritten =
                block_owner.resolve_outer_rewrites(operator.row_schema(), &resources.control())?;
            let rewritten_original = uqa_sql::binding::ExecutionQueryBlock::borrowed(original)
                .resolve_outer_rewrites(operator.row_schema(), &resources.control())?;
            Ok((rewritten, rewritten_original))
        })?;
    let block = rewritten.as_ref();
    let original = rewritten_original.as_ref();
    let residual = if pending {
        block.r#where.clone()
    } else {
        residual
    };
    let rewritten_scope;
    let (operator, ctes) = if rewrite_subqueries {
        let (operator, scope) =
            crate::physical::construction::OperatorBuild::run(operator, |_| {
                let arena_resources = context.ctes.production_resources()?;
                ctes.enter_scalar_subqueries_with_control(
                    &block.subqueries,
                    &arena_resources.control(),
                )
            })?;
        rewritten_scope = scope;
        (operator, &*rewritten_scope)
    } else {
        (operator, &*ctes)
    };
    let operator: Box<dyn crate::PhysicalOperator + 'a> = match outer_row {
        Some(outer) => Box::new(crate::ScopeOverlay::new(operator, outer)),
        None => operator,
    };
    // A volatile projection or filter observes only rows demanded by its parent, even when the underlying scan returns a larger batch.
    let operator: Box<dyn crate::PhysicalOperator + 'a> = if scalar_demand {
        Box::new(RowAtATime::new(operator))
    } else {
        operator
    };
    let (operator, columns) =
        crate::physical::construction::OperatorBuild::run(operator, |operator| {
            crate::query::projection::expand_from_star_columns(
                uqa_sql::semantics::projection_columns(&block.projections),
                &block.projections,
                operator.row_schema(),
            )
        })?;
    let (operator, resjunk) = crate::query::relational::build_relational_operator(
        context.relational,
        operator,
        residual,
        block,
        params,
        ctes,
        context.relational.runtime,
    )?;
    let operator = crate::query::relational::output::prepare_query_block_output_operator(
        context.relational,
        operator,
        original,
        params,
        ctes,
        &columns,
        resjunk,
        context.relational.runtime,
    )?;
    Ok(Some(operator))
}
