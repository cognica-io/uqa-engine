//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pass source aliases and placed predicates to the query rewrite owner.

use super::{qualifier_filter, QualifierFilters, QueryPlan, SQLError, SourceContext};

pub(super) fn specialize_lateral_source<S: Clone>(
    context: &SourceContext<'_, S>,
    source: &super::SourcePlan,
    filters: Option<&QualifierFilters>,
) -> Result<Option<super::SourcePlan>, SQLError> {
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
    let mut source = source.clone();
    let super::SourcePlan::Subquery { body, .. } = &mut source else {
        unreachable!()
    };
    **body = specialized;
    Ok(Some(source))
}

pub(super) fn specialize_source_filter<S: Clone>(
    context: &SourceContext<'_, S>,
    query: &QueryPlan,
    qualifier: &str,
    aliases: &[String],
    filters: Option<&QualifierFilters>,
) -> Result<Option<QueryPlan>, SQLError> {
    let Some(filter) = qualifier_filter(filters, qualifier) else {
        return Ok(None);
    };
    context
        .ctes
        .rewrites
        .push_derived_output_filter(query, qualifier, &filter, Some(aliases))
}
