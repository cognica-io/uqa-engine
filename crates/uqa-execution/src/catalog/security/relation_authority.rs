//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh authority retains private relation definitions and ACL tuples from the active transaction.

use crate::catalog::foreign::StoredForeignTable;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::security::BoundTableSecurity;
use uqa_storage::{
    catalog::relation_acl, CatalogFacade, StorageBackendError, StorageBackendResult,
};

pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    relation: &RelationIdentity,
    current: &BoundTableSecurity,
    mut latest: BoundTableSecurity,
) -> StorageBackendResult<BoundTableSecurity> {
    let Some(catalog) = catalog else {
        return Ok(latest);
    };
    if catalog.metadata_has_private_changes(&relation_acl::key(relation, None))? {
        latest.acl.clone_from(&current.acl);
        latest.acl_revisions.relation = current.acl_revisions.relation;
    }
    for (column, revision) in &current.acl_revisions.columns {
        if catalog.metadata_has_private_changes(&relation_acl::key(relation, Some(column)))? {
            if let Some(acl) = current.column_acls.get(column) {
                latest.column_acls.insert(column.clone(), acl.clone());
            } else {
                latest.column_acls.remove(column);
            }
            latest
                .acl_revisions
                .columns
                .insert(column.clone(), *revision);
        }
    }
    Ok(latest)
}

pub fn merge_private_views(
    catalog: Option<&dyn CatalogFacade>,
    current: &uqa_sql::catalog::stored_view::ViewDefinitions,
    mut latest: Arc<uqa_sql::catalog::stored_view::ViewDefinitions>,
) -> StorageBackendResult<Arc<uqa_sql::catalog::stored_view::ViewDefinitions>> {
    latest = merge_private_view_definitions(current, latest, |relation| {
        catalog.map_or(Ok(false), |catalog| {
            catalog.view_has_private_changes(relation)
        })
    })?;
    let mut next = (*latest).clone();
    for (relation, view) in latest.iter() {
        if let Some(previous) = current
            .get(relation)
            .filter(|previous| previous.object_id == view.object_id)
        {
            let security =
                merge_private(catalog, relation, &previous.security, view.security.clone())?;
            if security != view.security {
                let mut view = view.clone();
                view.security = security;
                next.insert(relation.clone(), view).map_err(|error| {
                    StorageBackendError::backend("merge private view authority", error)
                })?;
            }
        }
    }
    for (relation, view) in current
        .iter()
        .filter(|(_, view)| view.persistence == uqa_sql::ast::RelationPersistence::Temporary)
    {
        if !current.shares_definition(relation, &next) {
            next.insert(relation.clone(), view.clone())
                .map_err(|error| {
                    StorageBackendError::backend("retain temporary view definition", error)
                })?;
        }
    }
    Ok(Arc::new(next))
}

fn merge_private_view_definitions(
    current: &uqa_sql::catalog::stored_view::ViewDefinitions,
    mut latest: Arc<uqa_sql::catalog::stored_view::ViewDefinitions>,
    mut is_private: impl FnMut(&RelationIdentity) -> StorageBackendResult<bool>,
) -> StorageBackendResult<Arc<uqa_sql::catalog::stored_view::ViewDefinitions>> {
    let relations = current
        .keys()
        .chain(latest.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for relation in relations {
        if is_private(&relation)? {
            if let Some(view) = current.get(&relation) {
                if !current.shares_definition(&relation, &latest) {
                    Arc::make_mut(&mut latest)
                        .insert(relation, view.clone())
                        .map_err(|error| {
                            StorageBackendError::backend("merge private view definition", error)
                        })?;
                }
            } else {
                Arc::make_mut(&mut latest)
                    .remove(&relation)
                    .map_err(|error| {
                        StorageBackendError::backend("merge private view removal", error)
                    })?;
            }
        }
    }
    Ok(latest)
}

pub struct ForeignAuthoritySnapshot {
    pub definitions: Arc<BTreeMap<RelationIdentity, StoredForeignTable>>,
    pub security: Arc<BTreeMap<RelationIdentity, BoundTableSecurity>>,
}

pub fn merge_private_foreign(
    catalog: Option<&dyn CatalogFacade>,
    current_definitions: &BTreeMap<RelationIdentity, StoredForeignTable>,
    current: &BTreeMap<RelationIdentity, BoundTableSecurity>,
    mut latest_definitions: Arc<BTreeMap<RelationIdentity, StoredForeignTable>>,
    mut latest: Arc<BTreeMap<RelationIdentity, BoundTableSecurity>>,
) -> StorageBackendResult<ForeignAuthoritySnapshot> {
    merge_private_foreign_definitions(
        current_definitions,
        current,
        &mut latest_definitions,
        &mut latest,
        |relation| {
            catalog.map_or(Ok(false), |catalog| {
                catalog.foreign_table_has_private_changes(relation)
            })
        },
    )?;
    for (relation, security) in Arc::make_mut(&mut latest) {
        if current_definitions
            .get(relation)
            .zip(latest_definitions.get(relation))
            .is_some_and(|(previous, next)| previous.object_id == next.object_id)
        {
            if let Some(previous) = current.get(relation) {
                *security = merge_private(catalog, relation, previous, security.clone())?;
            }
        }
    }
    crate::catalog::foreign::restoration::merge_session_relations(
        current_definitions,
        current,
        Arc::make_mut(&mut latest_definitions),
        Arc::make_mut(&mut latest),
    )?;
    Ok(ForeignAuthoritySnapshot {
        definitions: latest_definitions,
        security: latest,
    })
}

fn merge_private_foreign_definitions(
    current_definitions: &BTreeMap<RelationIdentity, StoredForeignTable>,
    current_security: &BTreeMap<RelationIdentity, BoundTableSecurity>,
    latest_definitions: &mut Arc<BTreeMap<RelationIdentity, StoredForeignTable>>,
    latest_security: &mut Arc<BTreeMap<RelationIdentity, BoundTableSecurity>>,
    mut is_private: impl FnMut(&RelationIdentity) -> StorageBackendResult<bool>,
) -> StorageBackendResult<()> {
    let relations = current_definitions
        .keys()
        .chain(latest_definitions.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for relation in relations {
        if !is_private(&relation)? {
            continue;
        }
        if let Some(table) = current_definitions.get(&relation) {
            let security = current_security.get(&relation).ok_or_else(|| {
                uqa_storage::StorageBackendError::Other(format!(
                    "private foreign table `{}` has no security metadata",
                    relation.qualified_name()
                ))
            })?;
            Arc::make_mut(latest_definitions).insert(relation.clone(), table.clone());
            Arc::make_mut(latest_security).insert(relation, security.clone());
        } else {
            Arc::make_mut(latest_definitions).remove(&relation);
            Arc::make_mut(latest_security).remove(&relation);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
