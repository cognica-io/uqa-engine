//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog query preparation borrows source definitions and keeps mutable constructor owners through layout and output selection.

use super::{CteScope, SQLError, SQLParam, SourceContext};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{plan::MutableQueryPlan, RowSchema};

pub(in crate::query::sources) fn output_aliases(
    catalog: &[String],
    range: &[String],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut aliases = ProductionVec::new(*control);
    aliases.reserve(catalog.len().max(range.len()))?;
    for alias in range.iter().chain(catalog.iter().skip(range.len())) {
        aliases.push_produced(control.copy_text(alias)?)?;
    }
    Ok(aliases.finish()?)
}

pub(in crate::query::sources) fn restore_layouts<S: Clone>(
    context: &SourceContext<'_, S>,
    query: MutableQueryPlan,
    params: &[SQLParam],
    scope: &CteScope<S>,
    outer: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    uqa_sql::binding::restore_query_layouts_owned(
        context.ctes.routines,
        query,
        params,
        crate::query::binding::binding_context_with_control(scope, control)?,
        outer,
        control,
    )
}

pub(in crate::query::sources) struct QueryBinding<'a, S: Clone> {
    pub params: &'a [SQLParam],
    pub scope: &'a CteScope<S>,
    pub outer: Option<&'a RowSchema>,
}

pub(in crate::query::sources) fn prune_outputs<S: Clone>(
    context: &SourceContext<'_, S>,
    query: MutableQueryPlan,
    aliases: &[String],
    projection: &uqa_sql::plan::source_projection::SourceProjection,
    binding: QueryBinding<'_, S>,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let schema = uqa_sql::binding::production::bind_query_plan_schema_with_control(
        context.ctes.routines,
        &query,
        binding.params,
        crate::query::binding::binding_context_with_control(binding.scope, control)?,
        binding.outer,
        control,
    )?;
    context.ctes.rewrites.prune_owned_query_outputs(
        query,
        aliases,
        projection,
        &schema,
        &|name| binding.scope.contains_cte_name(name),
        control,
    )
}
