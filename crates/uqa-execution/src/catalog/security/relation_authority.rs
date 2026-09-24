//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fresh authority overlays only ACL tuples actually replaced by the retained transaction.

use crate::catalog::foreign::StoredForeignTable;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::{security::BoundTableSecurity, stored_view::StoredView};
use uqa_storage::{catalog::relation_acl, CatalogFacade, StorageBackendResult};

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
    current: &BTreeMap<RelationIdentity, StoredView>,
    mut latest: Arc<BTreeMap<RelationIdentity, StoredView>>,
) -> StorageBackendResult<Arc<BTreeMap<RelationIdentity, StoredView>>> {
    for (relation, view) in Arc::make_mut(&mut latest) {
        if let Some(previous) = current
            .get(relation)
            .filter(|previous| previous.object_id == view.object_id)
        {
            view.security =
                merge_private(catalog, relation, &previous.security, view.security.clone())?;
        }
    }
    Ok(latest)
}

pub fn merge_private_foreign(
    catalog: Option<&dyn CatalogFacade>,
    current_definitions: &BTreeMap<RelationIdentity, StoredForeignTable>,
    current: &BTreeMap<RelationIdentity, BoundTableSecurity>,
    latest_definitions: &BTreeMap<RelationIdentity, StoredForeignTable>,
    mut latest: Arc<BTreeMap<RelationIdentity, BoundTableSecurity>>,
) -> StorageBackendResult<Arc<BTreeMap<RelationIdentity, BoundTableSecurity>>> {
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
    Ok(latest)
}
