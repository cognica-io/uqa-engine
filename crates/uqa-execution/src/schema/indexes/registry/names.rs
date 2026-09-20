//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint and partition indexes share creation reservations with every relation kind.

use super::{BTreeMap, CatalogIndexRow, IndexRegistryContext, RelationIdentity};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub(super) fn reserve_new_names(
    context: &IndexRegistryContext<'_>,
    previous: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    candidate: &BTreeMap<RelationIdentity, CatalogIndexRow>,
) -> StorageBackendResult<()> {
    for relation in candidate
        .keys()
        .filter(|relation| !previous.contains_key(*relation))
    {
        crate::schema::namespaces::relation_names::reserve_relation_name(
            context.identities.locks,
            relation,
            || {
                let mut resolution = context.identities.session.relation_name_resolution();
                resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
                Ok(context
                    .identities
                    .catalog
                    .current_catalog_snapshot()
                    .relation_kind_resolution(&resolution, &relation.qualified_name())?
                    .into_found()
                    .is_some())
            },
        )
        .map_err(|error| StorageBackendError::backend("index name reservation", error))?;
    }
    Ok(())
}
