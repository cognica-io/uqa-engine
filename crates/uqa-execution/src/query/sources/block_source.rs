//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column projection, qualifier filters and row locks for a query block's physical FROM input.

use super::{CteScope, QualifierFilters, SourceContext, SourcePlan};
use crate::query::binding::sources::{ensure_select_with_sources, SourceBindings};
use crate::{PhysicalOperator, RowSchema};
use uqa_sql::{plan::QueryBlockPlan, SQLError, SQLParam};

pub(in crate::query) enum BlockSourceAccess<'s, 'p> {
    /// A materialized block checks SELECT after resolving its locks. Its planner may have removed the input.
    Query {
        bindings: Option<&'s SourceBindings<'p>>,
        outer: Option<&'s RowSchema>,
    },
    /// Derived queries have checked SELECT before validating expressions.
    Authorized(Option<&'s SourceBindings<'p>>),
    /// A tuple-local recheck rebuilds scans with the locked rows pinned.
    LockRecheck,
}

pub(in crate::query) fn build_block_source<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    block: &QueryBlockPlan,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    schema: &RowSchema,
    access: BlockSourceAccess<'_, '_>,
) -> Result<(Box<dyn PhysicalOperator + 'a>, Option<QualifierFilters>), SQLError> {
    let columns = context
        .planning
        .column_prune(block, from, ctes, Some(schema))?;
    let filters = context.planning.qualifier_filters(block, from, ctes)?;
    let locks = crate::query::locking::resolve_row_locks(
        context.locking,
        from,
        &block.locking,
        block.r#where.as_ref(),
        params,
        ctes,
    )?;
    let checked;
    let bindings = match access {
        BlockSourceAccess::Query { bindings, .. } => {
            checked = ensure_select_with_sources(block, from, ctes, bindings)?;
            checked.as_ref()
        }
        BlockSourceAccess::Authorized(bindings) => bindings,
        BlockSourceAccess::LockRecheck => None,
    };
    let mut scope = ctes.enter_source_row_locks_with_control(
        locks,
        &context.ctes.production_resources()?.control(),
    )?;
    let operator = match access {
        BlockSourceAccess::Query { outer, .. } if context.planning.block_input_is_empty(block) => {
            super::empty_source_operator(context, from, params, &scope, outer)?
        }
        BlockSourceAccess::LockRecheck => super::build_join_operator_with_recheck_pins(
            context,
            from,
            params,
            &mut scope,
            columns.as_ref(),
            filters.as_ref(),
        )?,
        _ => super::build_join_operator_with_bound_sources(
            context,
            from,
            params,
            &mut scope,
            columns.as_ref(),
            filters.as_ref(),
            bindings,
        )?,
    };
    Ok((operator, filters))
}
