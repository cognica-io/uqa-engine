//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned domain authority, complete restoration and durable registry publication.

use std::{collections::BTreeMap, ops::Deref, sync::Arc};
use uqa_sql::catalog::{
    domain::{validate_domain_registry, StoredDomain},
    roles::{guards::RoleCatalogGuards, RoleDefinition},
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

mod records;
mod restoration;
pub use restoration::{finish_restore, restore, DomainRestoreState, RestoredDomains};

pub const DOMAINS_METADATA_KEY: &str = "sql_domains_json";
pub type DomainRegistry = BTreeMap<String, StoredDomain>;
pub type DomainRegistryRead<'a> = Box<dyn Deref<Target = DomainRegistry> + 'a>;

pub trait DomainRegistryPublication: RoleCatalogGuards {
    fn domain_registry(&self) -> DomainRegistryRead<'_>;
    fn domain_catalog(&self) -> Option<&dyn CatalogFacade>;
    fn publish_domain_definitions(&self, registry: DomainRegistry);
}

/// Overlay private domain replacements and deletions without losing independently committed definitions.
pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &Arc<DomainRegistry>,
    committed: Arc<DomainRegistry>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<Arc<DomainRegistry>> {
    let committed = merge_private_records(current, committed, |name| {
        catalog.map_or(Ok(false), |catalog| {
            catalog.metadata_has_private_changes(&records::key(name))
        })
    })?;
    validate_domain_registry(&committed, roles).map_err(StorageBackendError::Other)?;
    Ok(committed)
}

fn merge_private_records(
    current: &DomainRegistry,
    mut committed: Arc<DomainRegistry>,
    mut is_private: impl FnMut(&str) -> StorageBackendResult<bool>,
) -> StorageBackendResult<Arc<DomainRegistry>> {
    let names = current
        .keys()
        .chain(committed.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for name in names {
        if is_private(&name)? {
            if let Some(domain) = current.get(&name) {
                Arc::make_mut(&mut committed).insert(name, domain.clone());
            } else {
                Arc::make_mut(&mut committed).remove(&name);
            }
        }
    }
    Ok(committed)
}

pub fn publish(
    publication: &dyn DomainRegistryPublication,
    before: &DomainRegistry,
    registry: DomainRegistry,
) -> Result<(), uqa_sql::SQLError> {
    let registry = {
        let current = publication.domain_registry();
        let registry = records::merge_changes(before, &registry, &current).map_err(|error| {
            uqa_sql::catalog::errors::storage_error("prepare domain catalog", &error)
        })?;
        validate_domain_registry(&registry, &publication.role_definitions())
            .map_err(uqa_sql::SQLError::Internal)?;
        if let Some(catalog) = publication.domain_catalog() {
            records::persist(catalog, &current, &registry).map_err(|error| {
                uqa_sql::catalog::errors::storage_error("persist domain catalog", &error)
            })?;
        }
        registry
    };
    publication.publish_domain_definitions(registry);
    Ok(())
}

#[cfg(test)]
mod tests;
