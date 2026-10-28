//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pass source aliases and placed predicates to the query rewrite owner.

use super::{qualifier_filter, QualifierFilters, QueryPlan, SQLError, SourceContext};

pub(super) struct SpecializedSource {
    source: super::SourcePlan,
    _memory: uqa_sql::plan::ExecutableMemory,
}

impl AsRef<super::SourcePlan> for SpecializedSource {
    fn as_ref(&self) -> &super::SourcePlan {
        &self.source
    }
}

impl std::ops::Deref for SpecializedSource {
    type Target = super::SourcePlan;
    fn deref(&self) -> &Self::Target {
        &self.source
    }
}

pub(super) fn specialize_lateral_source<S: Clone>(
    context: &SourceContext<'_, S>,
    source: &super::SourcePlan,
    filters: Option<&QualifierFilters>,
) -> Result<Option<SpecializedSource>, SQLError> {
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
    let (query, memory) = specialized.into_parts();
    *body = query;
    Ok(Some(SpecializedSource {
        source,
        _memory: memory,
    }))
}

pub(super) fn specialize_source_filter<S: Clone>(
    context: &SourceContext<'_, S>,
    query: &QueryPlan,
    qualifier: &str,
    aliases: &[String],
    filters: Option<&QualifierFilters>,
) -> Result<Option<uqa_sql::plan::OwnedQueryPlan>, SQLError> {
    let Some(filter) = qualifier_filter(filters, qualifier) else {
        return Ok(None);
    };
    context
        .ctes
        .rewrites
        .push_derived_output_filter(query, qualifier, &filter, Some(aliases))
}
