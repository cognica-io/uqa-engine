//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inspect named inputs without constructing executable CTE declarations.

use crate::query::scope::CteScope;
use uqa_core::{
    memory::{ProductionControl, ProductionVec},
    RelationIdentity, RelationName,
};
use uqa_sql::{
    catalog::{resolution::RelationLookupMode, view::StoredViewKind},
    plan::{QueryPlan, RelationalPlan, SourcePlan},
    SQLError,
};

pub(super) fn needed<S: Clone>(
    query: &QueryPlan,
    scope: &CteScope<S>,
    control: ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    scope.relation_lookup_mode().ok_or_else(|| {
        SQLError::Internal("query execution scope has no statement name-resolution snapshot".into())
    })?;
    let needed = inspect_query(query, scope, None, control)?;
    control.check()?;
    Ok(needed)
}

struct Names<'a> {
    local: &'a [RelationName<'a>],
    parent: Option<&'a Names<'a>>,
}

impl Names<'_> {
    fn contains(&self, name: RelationName<'_>) -> bool {
        let mut scope = Some(self);
        while let Some(current) = scope {
            if current.local.binary_search(&name).is_ok() {
                return true;
            }
            scope = current.parent;
        }
        false
    }
}

fn inspect_query<S: Clone>(
    query: &QueryPlan,
    scope: &CteScope<S>,
    parent: Option<&Names<'_>>,
    control: ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let mut names = ProductionVec::new(control);
    names.reserve(query.ctes.len())?;
    for cte in &query.ctes {
        names.push_copy(RelationName::decoded(&cte.name))?;
    }
    names.as_mut_slice().sort_unstable();
    let local = Names {
        local: &names,
        parent,
    };
    for cte in &query.ctes {
        if let Some(query) = cte.body.query() {
            if inspect_query(query, scope, Some(&local), control)? {
                return Ok(true);
            }
        }
    }
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                if inspect_source(source, query.relations_bound, scope, &local, control)? {
                    return Ok(true);
                }
            }
            inspect_children(&block.subqueries, scope, &local, control)
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            if inspect_query(left, scope, Some(&local), control)?
                || inspect_query(right, scope, Some(&local), control)?
            {
                return Ok(true);
            }
            inspect_children(subqueries, scope, &local, control)
        }
        RelationalPlan::Values { subqueries, .. } => {
            inspect_children(subqueries, scope, &local, control)
        }
    }
}

fn inspect_children<S: Clone>(
    queries: &[QueryPlan],
    scope: &CteScope<S>,
    names: &Names<'_>,
    control: ProductionControl<'_>,
) -> Result<bool, SQLError> {
    for query in queries {
        if inspect_query(query, scope, Some(names), control)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn inspect_source<S: Clone>(
    source: &SourcePlan,
    bound: bool,
    scope: &CteScope<S>,
    names: &Names<'_>,
    control: ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    Ok(match source {
        SourcePlan::Table {
            name,
            executable_query: None,
            ..
        } => {
            if RelationIdentity::borrowed_reference(name)
                .filter(|reference| reference.schema.is_none())
                .is_some_and(|reference| names.contains(reference.name))
            {
                true
            } else if scope.is_visible_cte(name) {
                scope.deferred_reference(name).is_some()
            } else {
                let mut resolution = scope.relation_name_resolution()?;
                resolution.set_lookup_mode(if bound {
                    RelationLookupMode::Bound
                } else {
                    RelationLookupMode::Dynamic
                });
                scope
                    .catalog_read_view()?
                    .view_resolved(&resolution, name)?
                    .is_some_and(|view| view.kind == StoredViewKind::View)
            }
        }
        SourcePlan::Join { left, right, .. } => {
            inspect_source(left, bound, scope, names, control)?
                || inspect_source(right, bound, scope, names, control)?
        }
        SourcePlan::Subquery { body, .. } => inspect_query(body, scope, Some(names), control)?,
        _ => false,
    })
}

#[cfg(test)]
mod tests;
