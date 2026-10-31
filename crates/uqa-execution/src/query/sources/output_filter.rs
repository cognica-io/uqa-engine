//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pass source aliases and placed predicates to the query rewrite owner.

use super::{QualifierFilters, QueryPlan, SQLError, SourceContext};
use uqa_core::memory::Produced;

pub(super) fn specialize_lateral_source<S: Clone>(
    context: &SourceContext<'_, S>,
    source: &super::SourcePlan,
    filters: Option<&QualifierFilters>,
) -> Result<Option<Produced<super::SourcePlan>>, SQLError> {
    let super::SourcePlan::Subquery {
        body,
        alias,
        column_aliases,
        ..
    } = source
    else {
        return Ok(None);
    };
    let Some(specialized) = specialize_source_filter(
        context,
        body,
        alias.as_deref().unwrap_or_default(),
        column_aliases,
        filters,
    )?
    else {
        return Ok(None);
    };
    let resources = context.ctes.production_resources()?;
    source
        .copy_with_query(specialized, &resources.control())
        .map(Some)
}

pub(super) fn specialize_source_filter<S: Clone>(
    context: &SourceContext<'_, S>,
    query: &QueryPlan,
    qualifier: &str,
    aliases: &[String],
    filters: Option<&QualifierFilters>,
) -> Result<Option<uqa_sql::plan::OwnedQueryPlan>, SQLError> {
    let resources = context.ctes.production_resources()?;
    let control = resources.control();
    let Some(filter) = uqa_sql::semantics::source_filters::qualifier_filter_with_control(
        filters, qualifier, &control,
    )?
    else {
        return Ok(None);
    };
    context
        .ctes
        .rewrites
        .push_derived_output_filter(query, qualifier, &filter, Some(aliases))
}
