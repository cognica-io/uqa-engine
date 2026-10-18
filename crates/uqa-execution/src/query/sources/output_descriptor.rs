//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Whether SQL's public descriptor also describes a derived query's physical output.

use crate::query::CteScope;
use uqa_sql::{
    catalog::{resolution::RelationLookupMode, view::StoredViewKind},
    plan::{CtePlan, QueryPlan, RelationalPlan, SourcePlan},
    semantics::{contains_retrieval, cte_reference_name},
    SQLError,
};

#[cfg(test)]
mod tests;

/// Retrieval attaches opaque score identities during physical assembly. Public SQL columns alone cannot bind a consumer of that output, even when their names and types are closed. Inspect the actual relation scopes, including folded and materialized CTEs and stored views, before choosing a deferred public descriptor.
pub(super) fn requires_physical_descriptor<S: Clone>(
    query: &QueryPlan,
    ctes: &CteScope<S>,
) -> Result<bool, SQLError> {
    if ctes
        .row_lock_outer_row()
        .is_some_and(|outer| outer.schema.score_sources().next().is_some())
    {
        return Ok(true);
    }
    DescriptorScope {
        ctes,
        local_ctes: Vec::new(),
        visiting: Vec::new(),
        completed: std::collections::BTreeMap::new(),
    }
    .query(query)
}

struct DescriptorScope<'a, S: Clone> {
    ctes: &'a CteScope<S>,
    local_ctes: Vec<&'a [CtePlan]>,
    visiting: Vec<*const QueryPlan>,
    completed: std::collections::BTreeMap<*const QueryPlan, bool>,
}

impl<'a, S: Clone> DescriptorScope<'a, S> {
    fn query(&mut self, query: &'a QueryPlan) -> Result<bool, SQLError> {
        let identity = std::ptr::from_ref(query);
        if let Some(required) = self.completed.get(&identity) {
            return Ok(*required);
        }
        if self.visiting.contains(&identity) {
            return Ok(false);
        }
        self.visiting.push(identity);
        self.local_ctes.push(&query.ctes);
        let result = match &query.root {
            RelationalPlan::QueryBlock(block) => {
                if block.r#where.as_ref().is_some_and(contains_retrieval) {
                    Ok(true)
                } else {
                    block.from.as_ref().map_or(Ok(false), |source| {
                        self.source(source, query.relations_bound)
                    })
                }
            }
            RelationalPlan::SetOp { left, right, .. } => {
                self.query(left)
                    .and_then(|left| if left { Ok(true) } else { self.query(right) })
            }
            RelationalPlan::Values { .. } => Ok(false),
        };
        self.local_ctes.pop();
        self.visiting.pop();
        if let Ok(required) = result {
            self.completed.insert(identity, required);
        }
        result
    }

    fn source(&mut self, source: &'a SourcePlan, bound: bool) -> Result<bool, SQLError> {
        match source {
            SourcePlan::Subquery { body, .. } => self.query(body),
            SourcePlan::Join { left, right, .. } => {
                Ok(self.source(left, bound)? || self.source(right, bound)?)
            }
            SourcePlan::Table { name, .. } => self.table(name, bound),
            SourcePlan::Values { .. }
            | SourcePlan::Function { .. }
            | SourcePlan::FunctionGroup { .. } => Ok(false),
        }
    }

    fn table(&mut self, name: &str, bound: bool) -> Result<bool, SQLError> {
        if let Some(reference) = cte_reference_name(name) {
            if let Some((scope, cte)) =
                self.local_ctes
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(scope, ctes)| {
                        ctes.iter()
                            .find(|cte| cte.name == reference)
                            .map(|cte| (scope, cte))
                    })
            {
                let hidden = self.local_ctes.split_off(scope + 1);
                let result = cte
                    .body
                    .query()
                    .map_or(Ok(false), |query| self.query(query));
                self.local_ctes.extend(hidden);
                return result;
            }
            if let Some(rows) = self.ctes.materialized_for_scan(name) {
                return Ok(rows.row_schema().score_sources().next().is_some());
            }
            if let Some(cte) = self.ctes.deferred_reference(name) {
                let hidden = std::mem::take(&mut self.local_ctes);
                let result = cte
                    .body
                    .query()
                    .map_or(Ok(false), |query| self.query(query));
                self.local_ctes = hidden;
                return result;
            }
            if self.ctes.is_visible_cte(name) {
                return Ok(false);
            }
        }
        let catalog = self.ctes.catalog_read_view()?;
        let mut resolution = self.ctes.relation_name_resolution()?;
        resolution.set_lookup_mode(if bound {
            RelationLookupMode::Bound
        } else {
            RelationLookupMode::Dynamic
        });
        let Some(view) = catalog.view_resolved(&resolution, name)? else {
            return Ok(false);
        };
        if view.kind == StoredViewKind::Materialized {
            return Ok(false);
        }
        // Stored views bind their own relation names; caller-local WITH definitions cannot shadow those identities.
        requires_physical_descriptor(&view.query, self.ctes)
    }
}
