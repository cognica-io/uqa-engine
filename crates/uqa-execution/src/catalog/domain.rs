//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned domain authority, complete restoration and durable registry publication.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Deref, sync::Arc};
use uqa_sql::catalog::{
    domain::{validate_domain_registry, StoredDomain},
    roles::{guards::RoleCatalogGuards, RoleDefinition},
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub const DOMAINS_METADATA_KEY: &str = "sql_domains_json";
pub type DomainRegistry = BTreeMap<String, StoredDomain>;
pub type DomainRegistryRead<'a> = Box<dyn Deref<Target = DomainRegistry> + 'a>;

pub trait DomainRegistryPublication: RoleCatalogGuards {
    fn domain_registry(&self) -> DomainRegistryRead<'_>;
    fn domain_catalog(&self) -> Option<&dyn CatalogFacade>;
    fn publish_domain_definitions(&self, registry: DomainRegistry);
}

/// Preserve the complete private metadata record, including deletions, without replacing untouched committed definitions with a stale session registry.
pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &Arc<DomainRegistry>,
    committed: Arc<DomainRegistry>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<Arc<DomainRegistry>> {
    let registry = if catalog.map_or(Ok(false), |catalog| {
        catalog.metadata_has_private_changes(DOMAINS_METADATA_KEY)
    })? {
        Arc::clone(current)
    } else {
        committed
    };
    validate_domain_registry(&registry, roles).map_err(StorageBackendError::Other)?;
    Ok(registry)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDomainCatalog<Domains> {
    domain_catalog_format: u32,
    domains: Domains,
}

pub fn encode(
    registry: &DomainRegistry,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<String> {
    validate_domain_registry(registry, roles).map_err(StorageBackendError::Other)?;
    Ok(serde_json::to_string(&StoredDomainCatalog {
        domain_catalog_format: 1,
        domains: registry,
    })?)
}

pub fn publish(
    publication: &dyn DomainRegistryPublication,
    registry: DomainRegistry,
) -> Result<(), uqa_sql::SQLError> {
    let catalog = publication.domain_catalog();
    let json = {
        let roles = publication.role_definitions();
        if catalog.is_some() {
            Some(encode(&registry, &roles).map_err(|error| {
                uqa_sql::catalog::errors::storage_error("serialize domain catalog", &error)
            })?)
        } else {
            validate_domain_registry(&registry, &roles).map_err(uqa_sql::SQLError::Internal)?;
            None
        }
    };
    if let Some(catalog) = catalog {
        catalog
            .set_metadata(
                DOMAINS_METADATA_KEY,
                json.as_deref().expect("durable domain catalog"),
            )
            .map_err(|error| {
                uqa_sql::catalog::errors::storage_error("persist domain catalog", &error)
            })?;
    }
    publication.publish_domain_definitions(registry);
    Ok(())
}

pub fn restore(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<DomainRegistry> {
    let json = catalog.get_metadata(DOMAINS_METADATA_KEY)?;
    let value = json
        .as_deref()
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()?;
    if value
        .as_ref()
        .is_some_and(|value| value.get("domain_catalog_format").is_some())
    {
        let stored: StoredDomainCatalog<DomainRegistry> =
            serde_json::from_value(value.expect("versioned domain catalog exists"))?;
        if stored.domain_catalog_format != 1 {
            return Err(StorageBackendError::Other(format!(
                "unsupported domain catalog format {}",
                stored.domain_catalog_format
            )));
        }
        validate_domain_registry(&stored.domains, roles).map_err(StorageBackendError::Other)?;
        return Ok(stored.domains);
    }
    if !allow_migration {
        return Err(StorageBackendError::Other(
            "domain authority requires initial catalog migration".into(),
        ));
    }
    let legacy: BTreeMap<String, StoredDomain<String>> = value
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let registry = legacy
        .into_iter()
        .map(|(name, domain)| {
            domain
                .bind_owner(roles)
                .map(|domain| (name, domain))
                .map_err(StorageBackendError::Other)
        })
        .collect::<StorageBackendResult<DomainRegistry>>()?;
    catalog.set_metadata(DOMAINS_METADATA_KEY, &encode(&registry, roles)?)?;
    Ok(registry)
}

#[cfg(test)]
mod tests;
