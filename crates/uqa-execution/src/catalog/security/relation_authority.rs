//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh authority retains private relation definitions and ACL tuples from the active transaction.

use crate::catalog::foreign::ForeignCatalog;
use uqa_core::memory::RetainedValue;
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
    mut latest: RetainedValue<uqa_sql::catalog::stored_view::ViewDefinitions>,
) -> StorageBackendResult<RetainedValue<uqa_sql::catalog::stored_view::ViewDefinitions>> {
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
    Ok(RetainedValue::new(next))
}

fn merge_private_view_definitions(
    current: &uqa_sql::catalog::stored_view::ViewDefinitions,
    mut latest: RetainedValue<uqa_sql::catalog::stored_view::ViewDefinitions>,
    mut is_private: impl FnMut(&RelationIdentity) -> StorageBackendResult<bool>,
) -> StorageBackendResult<RetainedValue<uqa_sql::catalog::stored_view::ViewDefinitions>> {
    let relations = current
        .keys()
        .chain(latest.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for relation in relations {
        if is_private(&relation)? {
            if let Some(view) = current.get(&relation) {
                if !current.shares_definition(&relation, &latest) {
                    RetainedValue::make_mut(&mut latest)
                        .insert(relation, view.clone())
                        .map_err(|error| {
                            StorageBackendError::backend("merge private view definition", error)
                        })?;
                }
            } else {
                RetainedValue::make_mut(&mut latest)
                    .remove(&relation)
                    .map_err(|error| {
                        StorageBackendError::backend("merge private view removal", error)
                    })?;
            }
        }
    }
    Ok(latest)
}

pub fn merge_private_foreign(
    catalog: Option<&dyn CatalogFacade>,
    current: &ForeignCatalog,
    latest: RetainedValue<ForeignCatalog>,
) -> StorageBackendResult<RetainedValue<ForeignCatalog>> {
    let mut next = (*latest).clone();
    merge_private_foreign_definitions(current, &mut next, |relation| {
        catalog.map_or(Ok(false), |catalog| {
            catalog.foreign_table_has_private_changes(relation)
        })
    })?;
    let authority = next.clone();
    for (relation, security) in authority.security().iter() {
        if current
            .tables()
            .get(relation)
            .zip(authority.tables().get(relation))
            .is_some_and(|(previous, table)| previous.object_id == table.object_id)
        {
            let merged = merge_private(
                catalog,
                relation,
                &current.security()[relation],
                security.clone(),
            )?;
            if &merged != security {
                next.replace_security(relation, merged).map_err(|error| {
                    StorageBackendError::backend("merge private foreign authority", error)
                })?;
            }
        }
    }
    crate::catalog::foreign::restoration::merge_session_relations(current, &mut next)?;
    Ok(RetainedValue::new(next))
}

fn merge_private_foreign_definitions(
    current: &ForeignCatalog,
    latest: &mut ForeignCatalog,
    mut is_private: impl FnMut(&RelationIdentity) -> StorageBackendResult<bool>,
) -> StorageBackendResult<()> {
    let relations = current
        .tables()
        .keys()
        .chain(latest.tables().keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut next = latest.clone();
    for relation in relations {
        if is_private(&relation)? {
            if let Some(table) = current.tables().get(&relation) {
                next.insert(
                    relation.clone(),
                    table.clone(),
                    current.security()[&relation].clone(),
                )
                .map_err(|error| {
                    StorageBackendError::backend("merge private foreign definition", error)
                })?;
            } else {
                next.remove(&relation).map_err(|error| {
                    StorageBackendError::backend("merge private foreign removal", error)
                })?;
            }
        }
    }
    *latest = next;
    Ok(())
}

#[cfg(test)]
mod tests;
