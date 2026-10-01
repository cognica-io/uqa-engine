//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned composite type authority: per-type records, public OID claims, private overlays and durable publication.

use std::{collections::BTreeMap, ops::Deref, sync::Arc};
use uqa_sql::catalog::{
    composite_type::{validate_composite_registry, StoredComposite},
    roles::RoleDefinition,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

mod descriptors;
mod records;

pub use descriptors::CompositeDescriptorCache;

/// Registered standalone composite types keyed by qualified type name.
pub type CompositeRegistry = BTreeMap<String, StoredComposite>;
pub type CompositeRegistryRead<'a> = Box<dyn Deref<Target = CompositeRegistry> + 'a>;

pub trait CompositeRegistryPublication {
    fn composite_registry(&self) -> CompositeRegistryRead<'_>;
    fn composite_catalog(&self) -> Option<&dyn CatalogFacade>;
    fn composite_role_definitions(&self) -> BTreeMap<String, RoleDefinition>;
    fn publish_composite_definitions(&self, registry: CompositeRegistry);
}

/// Read and validate every composite definition. Records and claims must agree before any definition is exposed.
pub fn restore(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<CompositeRegistry> {
    let registry = records::FORMAT.read(catalog)?;
    validate_composite_registry(&registry, roles).map_err(StorageBackendError::Other)?;
    Ok(registry)
}

/// Overlay private composite replacements and deletions without losing independently committed definitions.
pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &Arc<CompositeRegistry>,
    mut committed: Arc<CompositeRegistry>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<Arc<CompositeRegistry>> {
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
    validate_composite_registry(&committed, roles).map_err(StorageBackendError::Other)?;
    Ok(committed)
}

/// Merge this statement's changes onto the current registry, persist the changed records and claims, then publish.
pub fn publish(
    publication: &dyn CompositeRegistryPublication,
    before: &CompositeRegistry,
    registry: CompositeRegistry,
) -> Result<(), uqa_sql::SQLError> {
    let registry = {
        let current = publication.composite_registry();
        let registry = records::FORMAT
            .merge_changes(before, &registry, &current)
            .map_err(|error| {
                uqa_sql::catalog::errors::storage_error("prepare composite catalog", &error)
            })?;
        validate_composite_registry(&registry, &publication.composite_role_definitions())
            .map_err(uqa_sql::SQLError::Internal)?;
        if let Some(catalog) = publication.composite_catalog() {
            records::FORMAT
                .persist(catalog, &current, &registry)
                .map_err(|error| {
                    uqa_sql::catalog::errors::storage_error("persist composite catalog", &error)
                })?;
        }
        registry
    };
    publication.publish_composite_definitions(registry);
    Ok(())
}
