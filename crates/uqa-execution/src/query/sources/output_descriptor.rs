//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Whether SQL's public descriptor also describes a derived query's physical output.

use crate::query::CteScope;
use uqa_core::{
    memory::{OwnedMap, ProductionControl},
    RelationIdentity, RelationName,
};
use uqa_sql::{
    catalog::{resolution::RelationLookupMode, view::StoredViewKind},
    plan::{CtePlan, QueryPlan, RelationalPlan, SourcePlan},
    semantics::contains_retrieval,
    SQLError,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) fn requires_physical_descriptor<S: Clone>(
    query: &QueryPlan,
    ctes: &CteScope<S>,
) -> Result<bool, SQLError> {
    requires_physical_descriptor_with_control(query, ctes, ProductionControl::uncontrolled())
}

/// Retrieval attaches opaque score identities during physical assembly. Public SQL columns alone cannot bind a consumer of that output. Borrow lexical frames while admitting the completed-query index and selected catalog observations.
pub(super) fn requires_physical_descriptor_with_control<S: Clone>(
    query: &QueryPlan,
    ctes: &CteScope<S>,
    control: ProductionControl<'_>,
) -> Result<bool, SQLError> {
    inspect_scope(query, ctes, control, None)
}

fn inspect_scope<S: Clone>(
    query: &QueryPlan,
    ctes: &CteScope<S>,
    control: ProductionControl<'_>,
    visiting: Option<&Visiting<'_>>,
) -> Result<bool, SQLError> {
    control.check()?;
    if ctes
        .row_lock_outer_row()
        .is_some_and(|outer| outer.schema.score_sources().next().is_some())
    {
        return Ok(true);
    }
    DescriptorScope {
        ctes,
        control,
        completed: OwnedMap::new(),
    }
    .query(query, None, visiting)
}

struct LocalDefinitions<'a> {
    declarations: &'a [CtePlan],
    parent: Option<&'a Self>,
}

impl LocalDefinitions<'_> {
    fn find(&self, reference: RelationName<'_>) -> Option<(&Self, usize)> {
        let mut frame = Some(self);
        while let Some(current) = frame {
            if let Some(position) = current
                .declarations
                .iter()
                .position(|cte| RelationName::decoded(&cte.name) == reference)
            {
                return Some((current, position));
            }
            frame = current.parent;
        }
        None
    }
}

struct Visiting<'a> {
    identity: usize,
    parent: Option<&'a Self>,
}

impl Visiting<'_> {
    fn contains(&self, identity: usize) -> bool {
        let mut frame = Some(self);
        while let Some(current) = frame {
            if current.identity == identity {
                return true;
            }
            frame = current.parent;
        }
        false
    }
}

struct DescriptorScope<'a, 'c, S: Clone> {
    ctes: &'a CteScope<S>,
    control: ProductionControl<'c>,
    completed: OwnedMap<usize, bool>,
}

impl<S: Clone> DescriptorScope<'_, '_, S> {
    fn query(
        &mut self,
        query: &QueryPlan,
        definitions: Option<&LocalDefinitions<'_>>,
        visiting: Option<&Visiting<'_>>,
    ) -> Result<bool, SQLError> {
        self.control.check()?;
        let identity = std::ptr::from_ref(query) as usize;
        if let Some(required) = self.completed.get(&identity) {
            return Ok(*required);
        }
        if visiting.is_some_and(|visiting| visiting.contains(identity)) {
            return Ok(false);
        }
        let visiting = Visiting {
            identity,
            parent: visiting,
        };
        let definitions = LocalDefinitions {
            declarations: &query.ctes,
            parent: definitions,
        };
        let required = match &query.root {
            RelationalPlan::QueryBlock(block) => {
                if block.r#where.as_ref().is_some_and(contains_retrieval) {
                    true
                } else {
                    block.from.as_ref().map_or(Ok(false), |source| {
                        self.source(source, query.relations_bound, Some(&definitions), &visiting)
                    })?
                }
            }
            RelationalPlan::SetOp { left, right, .. } => {
                self.query(left, Some(&definitions), Some(&visiting))?
                    || self.query(right, Some(&definitions), Some(&visiting))?
            }
            RelationalPlan::Values { .. } => false,
        };
        self.control.check()?;
        match self.control.budget() {
            Some(budget) => {
                self.completed.insert_budgeted(identity, required, budget)?;
            }
            None => {
                self.completed.insert(identity, required);
            }
        }
        Ok(required)
    }

    fn source(
        &mut self,
        source: &SourcePlan,
        bound: bool,
        definitions: Option<&LocalDefinitions<'_>>,
        visiting: &Visiting<'_>,
    ) -> Result<bool, SQLError> {
        self.control.check()?;
        match source {
            SourcePlan::RetainedRows(_) => Ok(true),
            SourcePlan::Subquery { body, .. } => self.query(body, definitions, Some(visiting)),
            SourcePlan::Join { left, right, .. } => {
                Ok(self.source(left, bound, definitions, visiting)?
                    || self.source(right, bound, definitions, visiting)?)
            }
            SourcePlan::Table { name, .. } => self.table(name, bound, definitions, visiting),
            SourcePlan::Values { .. }
            | SourcePlan::Function { .. }
            | SourcePlan::FunctionGroup { .. } => Ok(false),
        }
    }

    fn table(
        &mut self,
        name: &str,
        bound: bool,
        definitions: Option<&LocalDefinitions<'_>>,
        visiting: &Visiting<'_>,
    ) -> Result<bool, SQLError> {
        if let Some(reference) = RelationIdentity::borrowed_reference(name)
            .filter(|reference| reference.schema.is_none())
        {
            if let Some((frame, position)) =
                definitions.and_then(|frame| frame.find(reference.name))
            {
                let cte = &frame.declarations[position];
                let definitions = LocalDefinitions {
                    declarations: if cte.recursive {
                        frame.declarations
                    } else {
                        &frame.declarations[..position]
                    },
                    parent: frame.parent,
                };
                return cte.body.query().map_or(Ok(false), |query| {
                    self.query(query, Some(&definitions), Some(visiting))
                });
            }
            if let Some(rows) = self.ctes.materialized_for_scan(name) {
                return Ok(rows.row_schema().score_sources().next().is_some());
            }
            if let Some(cte) = self.ctes.deferred_reference(name) {
                if let Some(definition) = self
                    .ctes
                    .definition_scope_with_control(name, &self.control)?
                {
                    return cte.body.query().map_or(Ok(false), |query| {
                        inspect_scope(query, &definition, self.control, Some(visiting))
                    });
                }
                return cte
                    .body
                    .query()
                    .map_or(Ok(false), |query| self.query(query, None, Some(visiting)));
            }
            if self.ctes.is_visible_cte(name) {
                return Ok(false);
            }
        }
        let catalog = self.ctes.catalog_read_view()?;
        let (mut resolution, _memory) = self
            .ctes
            .relation_name_resolution_with_control(&self.control)?
            .into_parts();
        resolution.set_lookup_mode(if bound {
            RelationLookupMode::Bound
        } else {
            RelationLookupMode::Dynamic
        });
        let Some(view) = catalog.view_resolved_with_control(&resolution, name, &self.control)?
        else {
            return Ok(false);
        };
        if view.kind == StoredViewKind::Materialized {
            return Ok(false);
        }
        // Stored views bind their own relation names; caller-local WITH definitions cannot shadow those identities.
        inspect_scope(&view.query, self.ctes, self.control, Some(visiting))
    }
}
