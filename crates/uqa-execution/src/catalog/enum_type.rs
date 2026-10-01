//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned enum authority: per-type records, public OID claims, private overlays and durable publication.

use std::{collections::BTreeMap, ops::Deref, sync::Arc};
use uqa_sql::catalog::{
    enum_type::{validate_enum_registry, StoredEnum},
    roles::RoleDefinition,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

mod labels;
mod records;

pub use labels::EnumLabelCache;

/// Registered enum types keyed by qualified type name.
pub type EnumRegistry = BTreeMap<String, StoredEnum>;
pub type EnumRegistryRead<'a> = Box<dyn Deref<Target = EnumRegistry> + 'a>;

pub trait EnumRegistryPublication {
    fn enum_registry(&self) -> EnumRegistryRead<'_>;
    fn enum_catalog(&self) -> Option<&dyn CatalogFacade>;
    fn enum_role_definitions(&self) -> BTreeMap<String, RoleDefinition>;
    fn publish_enum_definitions(&self, registry: EnumRegistry);
}

/// Read and validate every enum definition. Records and claims must agree before any definition is exposed.
pub fn restore(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<EnumRegistry> {
    let registry = records::FORMAT.read(catalog)?;
    validate_enum_registry(&registry, roles).map_err(StorageBackendError::Other)?;
    Ok(registry)
}

/// Overlay private enum replacements and deletions without losing independently committed definitions.
pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &Arc<EnumRegistry>,
    mut committed: Arc<EnumRegistry>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<Arc<EnumRegistry>> {
    let names = current
        .keys()
        .chain(committed.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for name in names {
        let private = catalog.map_or(Ok(false), |catalog| {
            catalog.metadata_has_private_changes(&records::FORMAT.key(&name))
        })?;
        if private {
            if let Some(definition) = current.get(&name) {
                Arc::make_mut(&mut committed).insert(name, definition.clone());
            } else {
                Arc::make_mut(&mut committed).remove(&name);
            }
        }
    }
    validate_enum_registry(&committed, roles).map_err(StorageBackendError::Other)?;
    Ok(committed)
}

/// Merge this statement's changes onto the current registry, persist the changed records and claims, then publish.
pub fn publish(
    publication: &dyn EnumRegistryPublication,
    before: &EnumRegistry,
    registry: EnumRegistry,
) -> Result<(), uqa_sql::SQLError> {
    let registry = {
        let current = publication.enum_registry();
        let registry = records::FORMAT
            .merge_changes(before, &registry, &current)
            .map_err(|error| {
                uqa_sql::catalog::errors::storage_error("prepare enum catalog", &error)
            })?;
        validate_enum_registry(&registry, &publication.enum_role_definitions())
            .map_err(uqa_sql::SQLError::Internal)?;
        if let Some(catalog) = publication.enum_catalog() {
            records::FORMAT
                .persist(catalog, &current, &registry)
                .map_err(|error| {
                    uqa_sql::catalog::errors::storage_error("persist enum catalog", &error)
                })?;
        }
        registry
    };
    publication.publish_enum_definitions(registry);
    Ok(())
}

#[cfg(test)]
mod tests;
