//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transfer complete SELECT coverage only within its original native authority.

use super::{CteScope, QueryBlockPlan, SQLError, SourceBindings, SourcePlan};
use uqa_sql::{
    catalog::{resolution::RelationLookupMode, roles::RoleReference},
    semantics::privileges::coverage::SelectSourceCoverage,
};

#[derive(Clone)]
pub(super) struct SourceAuthorization<'a> {
    coverage: SelectSourceCoverage<'a>,
    subject: RoleReference,
    lookup_mode: RelationLookupMode,
}

impl SourceAuthorization<'_> {
    fn matches<S: Clone>(&self, ctes: &CteScope<S>) -> bool {
        ctes.privilege_subject().ok() == Some(&self.subject)
            && ctes.relation_lookup_mode() == Some(self.lookup_mode)
    }

    pub(super) fn join_child(&self, right: bool) -> Option<Self> {
        Some(Self {
            coverage: self.coverage.join_child(right)?,
            ..self.clone()
        })
    }

    pub(super) fn query_source<S: Clone>(
        &self,
        block: &QueryBlockPlan,
        ctes: &CteScope<S>,
    ) -> Option<Self> {
        self.matches(ctes).then_some(())?;
        Some(Self {
            coverage: self.coverage.query_source(block)?,
            ..self.clone()
        })
    }
}

pub(crate) fn ensure_select_with_sources<'a, S: Clone>(
    block: &'a QueryBlockPlan,
    source: &'a SourcePlan,
    ctes: &CteScope<S>,
    bindings: Option<&SourceBindings<'a>>,
) -> Result<Option<SourceBindings<'a>>, SQLError> {
    let eligible =
        bindings.filter(|bindings| bindings.matches_source(source) && bindings.scope.matches(ctes));
    if eligible.is_some_and(|bindings| {
        bindings.authorization.as_ref().is_some_and(|checked| {
            checked.matches(ctes) && checked.coverage.covers_query(block, source)
        })
    }) {
        return Ok(bindings.cloned());
    }
    let coverage = crate::query::privileges::check_query_source(block, source, ctes)?;
    let authorization = ctes
        .privilege_subject()
        .ok()
        .zip(ctes.relation_lookup_mode());
    Ok(bindings.map(|bindings| SourceBindings {
        authorization: eligible.and(authorization).map(|(subject, lookup_mode)| {
            SourceAuthorization {
                coverage,
                subject: subject.clone(),
                lookup_mode,
            }
        }),
        ..bindings.clone()
    }))
}

#[cfg(test)]
mod tests;
