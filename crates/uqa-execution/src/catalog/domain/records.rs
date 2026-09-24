//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent domain definitions and their public OID claims.

use super::{DomainRegistry, DOMAINS_METADATA_KEY};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uqa_sql::catalog::{domain::StoredDomain, roles::RoleDefinition};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub(super) const PREFIX: &str = "uqa.sql.domain.v1:";
const OID_PREFIX: &str = "uqa.sql.domain_oid.v1:";
const FORMAT: &str = r#"{"domain_catalog_format":3}"#;

pub(super) fn key(name: &str) -> String {
    format!("{PREFIX}{name}")
}

fn oid_key(oid: u32) -> String {
    format!("{OID_PREFIX}{oid}")
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Aggregate {
    domain_catalog_format: u32,
    domains: DomainRegistry,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordFormat {
    domain_catalog_format: u32,
}

pub(super) fn read(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<(DomainRegistry, bool)> {
    let value = catalog
        .get_metadata(DOMAINS_METADATA_KEY)?
        .map(|json| serde_json::from_str::<serde_json::Value>(&json))
        .transpose()?;
    let version = value
        .as_ref()
        .and_then(|value| value.get("domain_catalog_format"))
        .and_then(serde_json::Value::as_u64);
    if matches!(version, Some(2 | 3)) {
        let marker: RecordFormat = serde_json::from_value(value.expect("domain format marker"))?;
        let current = marker.domain_catalog_format == 3;
        if !current && !allow_migration {
            return Err(StorageBackendError::Other(
                "domain constraints require initial catalog migration".into(),
            ));
        }
        let mut registry = DomainRegistry::new();
        for (record, json) in catalog.metadata_with_prefix(PREFIX)? {
            let name = record.strip_prefix(PREFIX).expect("domain record prefix");
            registry.insert(name.to_owned(), serde_json::from_str(&json)?);
        }
        validate_oids(catalog, &registry)?;
        return Ok((registry, current));
    }
    if !catalog.metadata_with_prefix(PREFIX)?.is_empty()
        || !catalog.metadata_with_prefix(OID_PREFIX)?.is_empty()
    {
        return Err(StorageBackendError::Other(
            "domain records exist without their format marker".into(),
        ));
    }
    if !allow_migration {
        return Err(StorageBackendError::Other(
            "domain authority requires initial catalog migration".into(),
        ));
    }
    if value
        .as_ref()
        .is_some_and(|value| value.get("domain_catalog_format").is_some())
    {
        let stored: Aggregate = serde_json::from_value(value.expect("legacy aggregate"))?;
        if stored.domain_catalog_format != 1 {
            return Err(StorageBackendError::Other(format!(
                "unsupported domain catalog format {}",
                stored.domain_catalog_format
            )));
        }
        return Ok((stored.domains, false));
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
    Ok((registry, false))
}

fn validate_oids(
    catalog: &dyn CatalogFacade,
    registry: &DomainRegistry,
) -> StorageBackendResult<()> {
    let claims = catalog.metadata_with_prefix(OID_PREFIX)?;
    if claims.len() != registry.len()
        || claims.iter().any(|(key, name)| {
            registry
                .get(name)
                .is_none_or(|domain| *key != oid_key(domain.oid))
        })
    {
        return Err(StorageBackendError::Other(
            "domain OID records do not match domain definitions".into(),
        ));
    }
    Ok(())
}

/// Initial restoration owns the transaction; every domain is validated before this writes any record.
pub(super) fn migrate(
    catalog: &dyn CatalogFacade,
    registry: &DomainRegistry,
) -> StorageBackendResult<()> {
    for (name, domain) in registry {
        catalog.set_metadata(&key(name), &serde_json::to_string(domain)?)?;
        catalog.set_metadata(&oid_key(domain.oid), name)?;
    }
    catalog.set_metadata(DOMAINS_METADATA_KEY, FORMAT)
}

pub(super) fn merge_changes(
    before: &DomainRegistry,
    after: &DomainRegistry,
    current: &DomainRegistry,
) -> StorageBackendResult<DomainRegistry> {
    let names = before
        .keys()
        .chain(after.keys())
        .collect::<std::collections::BTreeSet<_>>();
    let mut merged = current.clone();
    for name in names {
        let previous = before.get(name).map(serde_json::to_string).transpose()?;
        let next = after.get(name).map(serde_json::to_string).transpose()?;
        if previous == next {
            continue;
        }
        if previous != current.get(name).map(serde_json::to_string).transpose()? {
            return Err(StorageBackendError::backend(
                "domain definition",
                uqa_sql::SQLError::Routine {
                    sqlstate: "40001".into(),
                    message: format!("domain `{name}` changed during catalog publication"),
                },
            ));
        }
        if let Some(domain) = after.get(name) {
            merged.insert(name.clone(), domain.clone());
        } else {
            merged.remove(name);
        }
    }
    Ok(merged)
}

pub(super) fn persist(
    catalog: &dyn CatalogFacade,
    before: &DomainRegistry,
    after: &DomainRegistry,
) -> StorageBackendResult<()> {
    let mut changed = Vec::new();
    for (name, domain) in after {
        let json = serde_json::to_string(domain)?;
        if before
            .get(name)
            .map(serde_json::to_string)
            .transpose()?
            .as_ref()
            == Some(&json)
        {
            continue;
        }
        if before
            .get(name)
            .is_none_or(|old| old.object_id != domain.object_id)
        {
            if let Some(claimed) = catalog.get_metadata(&oid_key(domain.oid))? {
                if before
                    .get(&claimed)
                    .is_none_or(|old| old.object_id != domain.object_id)
                {
                    return Err(StorageBackendError::backend(
                        "domain OID",
                        uqa_sql::SQLError::Routine {
                            sqlstate: "23505".into(),
                            message: format!("domain OID {} is already assigned", domain.oid),
                        },
                    ));
                }
            }
        }
        changed.push((name, domain, json));
    }
    for (name, domain) in before {
        if after.get(name).is_none_or(|next| next.oid != domain.oid) {
            catalog.delete_metadata(&oid_key(domain.oid))?;
        }
        if !after.contains_key(name) {
            catalog.delete_metadata(&key(name))?;
        }
    }
    for (name, domain, json) in changed {
        if before.get(name).is_none_or(|old| old.oid != domain.oid) {
            catalog.set_metadata(&oid_key(domain.oid), name)?;
        }
        catalog.set_metadata(&key(name), &json)?;
    }
    Ok(())
}
