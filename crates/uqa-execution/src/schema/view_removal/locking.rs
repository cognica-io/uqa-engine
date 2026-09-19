//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lock dependent view identities and recheck each dependency before following it.

use crate::{
    catalog::view::ViewRegistryState,
    row_locks::{
        binding::{bind_relation, RelationBinding, RelationLockSession},
        RelationLockMode,
    },
};
use std::collections::{BTreeSet, VecDeque};
use uqa_core::RelationIdentity;
use uqa_sql::{binding::view_dependencies::query_plan_references_relation, SQLError};

/// Callers retain definition locks on the supplied roots. Each retained child lock protects the next dependency traversal; a removed edge releases its provisional acquisition and never schedules that child for deletion.
pub fn lock_dependent_views(
    registry: &dyn ViewRegistryState,
    session: &dyn RelationLockSession,
    roots: &[String],
) -> Result<Vec<String>, SQLError> {
    let roots = roots
        .iter()
        .map(|name| RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal))
        .collect::<Result<Vec<_>, _>>()?;
    let mut seen = {
        let views = registry.views_read();
        roots
            .iter()
            .filter_map(|name| views.get(name).map(|view| view.object_id))
            .collect::<BTreeSet<_>>()
    };
    let mut pending = VecDeque::from(roots);
    let mut selected = Vec::new();
    let empty_ctes = BTreeSet::new();
    while let Some(parent) = pending.pop_front() {
        let candidates = {
            let views = registry.views_read();
            views
                .iter()
                .filter(|(name, view)| {
                    *name != &parent
                        && !seen.contains(&view.object_id)
                        && query_plan_references_relation(&view.query, &parent, &empty_ctes)
                })
                .map(|(_, view)| view.object_id)
                .collect::<Vec<_>>()
        };
        for object_id in candidates {
            let bound = bind_relation(
                session,
                RelationLockMode::AccessExclusive,
                false,
                || {
                    let views = registry.views_read();
                    Ok(views
                        .iter()
                        .find(|(_, view)| view.object_id == object_id)
                        .and_then(|(name, view)| {
                            query_plan_references_relation(&view.query, &parent, &empty_ctes).then(
                                || RelationBinding {
                                    name: name.qualified_name(),
                                    object_id: Some(object_id),
                                    value: name.clone(),
                                },
                            )
                        }))
                },
                |_| Ok(()),
            )?;
            if let Some(bound) = bound {
                seen.insert(object_id);
                pending.push_back(bound.value);
                selected.push(bound.name);
            }
        }
    }
    Ok(selected)
}

#[cfg(test)]
mod tests;
