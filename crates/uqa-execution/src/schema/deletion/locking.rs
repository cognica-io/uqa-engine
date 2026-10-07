//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `AcquireDeletionLock` for relations: every relation a deletion removes or changes is held in access exclusive mode, by its object identity, before anything is removed.

use super::plan::DeletionPlan;
use super::CatalogRemovalContext;
use crate::row_locks::{binding::lock_any_relation_identity, RelationLockMode};
use std::collections::BTreeSet;
use uqa_sql::SQLError;

/// Lock the foreign definitions reached by deletion and repeat the dependency search after a wait can change the catalog.
pub(super) fn lock_foreign_definitions(
    context: &CatalogRemovalContext<'_>,
    targets: &uqa_sql::catalog::dependencies::DeletionTargets,
    locked: &mut BTreeSet<uqa_sql::catalog::dependencies::ObjectAddress>,
) -> Result<bool, SQLError> {
    use crate::row_locks::shared_objects::SharedCatalogLock;
    use uqa_sql::catalog::dependencies::{FOREIGN_SERVER_CLASS, FOREIGN_WRAPPER_CLASS};
    let mut acquired = false;
    for target in targets.targets() {
        let object = target.object;
        if matches!(
            object.class_id,
            FOREIGN_SERVER_CLASS | FOREIGN_WRAPPER_CLASS
        ) && locked.insert(object)
        {
            context
                .shared_locks
                .acquire_shared_catalog(
                    SharedCatalogLock::Object {
                        class_id: object.class_id,
                        oid: object.object_id,
                    },
                    RelationLockMode::AccessExclusive,
                )?
                .retain();
            acquired = true;
        }
    }
    if acquired {
        context.shared_locks.refresh_shared_catalog()?;
        context.locks.prepare_definition_write()?;
    }
    Ok(acquired)
}

/// Lock the plan's relations that are not locked yet. Returns whether any lock was newly taken: waiting for one may have let other sessions change the catalog, so the caller searches again.
pub(super) fn lock_relations(
    context: &CatalogRemovalContext<'_>,
    plan: &DeletionPlan,
    locked: &mut BTreeSet<[u8; 16]>,
) -> Result<bool, SQLError> {
    // The identities are those the search saw: a wait for one lock can give another relation's name to a new relation.
    let mut identities = Vec::new();
    for relation in plan.relations() {
        let name = relation.qualified_name();
        // A relation that has already disappeared leaves the next search without it.
        if let Some(object_id) = context.identities.relation_object_id(&name)? {
            identities.push((name, object_id));
        }
    }
    let mut acquired = false;
    for (name, object_id) in identities {
        if locked.insert(object_id) {
            lock_any_relation_identity(
                context.identities,
                context.locks,
                name,
                object_id,
                RelationLockMode::AccessExclusive,
            )?;
            acquired = true;
        }
    }
    if acquired {
        context.locks.prepare_definition_write()?;
    }
    Ok(acquired)
}
