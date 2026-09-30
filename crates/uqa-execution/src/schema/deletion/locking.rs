//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `AcquireDeletionLock` for relations: every relation a deletion removes or changes is held in access exclusive mode before anything is removed.

use super::plan::DeletionPlan;
use super::CatalogRemovalContext;
use std::collections::BTreeSet;
use uqa_sql::SQLError;

/// Lock the plan's relations that are not locked yet. Returns whether any lock was newly taken: waiting for one may have let other sessions change the catalog, so the caller searches again.
pub(super) fn lock_relations(
    context: &CatalogRemovalContext<'_>,
    plan: &DeletionPlan,
    locked: &mut BTreeSet<String>,
) -> Result<bool, SQLError> {
    let names = plan
        .relations()
        .into_iter()
        .map(|relation| relation.qualified_name())
        .filter(|name| !locked.contains(name))
        .collect::<BTreeSet<_>>();
    if names.is_empty() {
        return Ok(false);
    }
    crate::row_locks::binding::prepare_dependent_relation_writes(
        context.locks,
        names.iter().map(String::as_str),
    )?;
    locked.extend(names);
    Ok(true)
}
