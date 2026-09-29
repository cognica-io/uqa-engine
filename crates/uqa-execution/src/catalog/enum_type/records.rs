//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent enum definitions and claims for every public OID they own.

use super::EnumRegistry;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::catalog::enum_type::StoredEnum;
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

const FORMAT_KEY: &str = "sql_enums_json";
const FORMAT: &str = r#"{"enum_catalog_format":1}"#;
const PREFIX: &str = "uqa.sql.enum.v1:";
const OID_PREFIX: &str = "uqa.sql.enum_oid.v1:";

pub(super) fn key(name: &str) -> String {
    format!("{PREFIX}{name}")
}

fn oid_key(oid: u32) -> String {
    format!("{OID_PREFIX}{oid}")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordFormat {
    enum_catalog_format: u32,
}

fn claimed_oids(definition: &StoredEnum) -> BTreeSet<u32> {
    [definition.oid, definition.array_oid]
        .into_iter()
        .chain(definition.label_oids())
        .collect()
}

fn corrupt(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

pub(super) fn read(catalog: &dyn CatalogFacade) -> StorageBackendResult<EnumRegistry> {
    let records = catalog.metadata_with_prefix(PREFIX)?;
    let claims = catalog.metadata_with_prefix(OID_PREFIX)?;
    match catalog.get_metadata(FORMAT_KEY)? {
        Some(marker) => {
            let marker: RecordFormat = serde_json::from_str(&marker)?;
            if marker.enum_catalog_format != 1 {
                return Err(corrupt(format!(
                    "unsupported enum catalog format {}",
                    marker.enum_catalog_format
                )));
            }
        }
        None if records.is_empty() && claims.is_empty() => return Ok(EnumRegistry::new()),
        None => return Err(corrupt("enum records exist without their format marker")),
    }
    let mut registry = EnumRegistry::new();
    for (record, json) in records {
        let name = record.strip_prefix(PREFIX).expect("enum record prefix");
        registry.insert(name.to_owned(), serde_json::from_str(&json)?);
    }
    let expected = registry
        .iter()
        .flat_map(|(name, definition)| {
            claimed_oids(definition)
                .into_iter()
                .map(move |oid| (oid_key(oid), name.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    if claims.into_iter().collect::<BTreeMap<_, _>>() != expected {
        return Err(corrupt("enum OID records do not match enum definitions"));
    }
    Ok(registry)
}

/// Apply this statement's changes to the current registry. A definition changed by another writer since the statement read it is a serialization failure rather than a silent overwrite.
pub(super) fn merge_changes(
    before: &EnumRegistry,
    after: &EnumRegistry,
    current: &EnumRegistry,
) -> StorageBackendResult<EnumRegistry> {
    let names = before.keys().chain(after.keys()).collect::<BTreeSet<_>>();
    let mut merged = current.clone();
    for name in names {
        let previous = before.get(name).map(serde_json::to_string).transpose()?;
        let next = after.get(name).map(serde_json::to_string).transpose()?;
        if previous == next {
            continue;
        }
        if previous != current.get(name).map(serde_json::to_string).transpose()? {
            return Err(StorageBackendError::backend(
                "enum definition",
                uqa_sql::SQLError::Routine {
                    sqlstate: "40001".into(),
                    message: format!("enum type `{name}` changed during catalog publication"),
                },
            ));
        }
        if let Some(definition) = after.get(name) {
            merged.insert(name.clone(), definition.clone());
        } else {
            merged.remove(name);
        }
    }
    Ok(merged)
}

/// Write changed definitions and their claims in the caller's transaction. Claims are keyed by OID, so a concurrently committed claim for the same OID conflicts with this write instead of aliasing another type.
pub(super) fn persist(
    catalog: &dyn CatalogFacade,
    before: &EnumRegistry,
    after: &EnumRegistry,
) -> StorageBackendResult<()> {
    if !after.is_empty() && catalog.get_metadata(FORMAT_KEY)?.is_none() {
        catalog.set_metadata(FORMAT_KEY, FORMAT)?;
    }
    let owned_before = before
        .iter()
        .flat_map(|(name, definition)| {
            claimed_oids(definition)
                .into_iter()
                .map(move |oid| (oid, name.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    let owned_after = after
        .iter()
        .flat_map(|(name, definition)| {
            claimed_oids(definition)
                .into_iter()
                .map(move |oid| (oid, name.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    for (oid, name) in &owned_after {
        if owned_before.get(oid) == Some(name) {
            continue;
        }
        if let Some(claimed) = catalog.get_metadata(&oid_key(*oid))? {
            if owned_before.get(oid) != Some(&claimed.as_str()) {
                return Err(StorageBackendError::backend(
                    "enum OID",
                    uqa_sql::SQLError::Routine {
                        sqlstate: "23505".into(),
                        message: format!("enum OID {oid} is already assigned"),
                    },
                ));
            }
        }
    }
    for (oid, name) in &owned_before {
        if owned_after.get(oid) != Some(name) {
            catalog.delete_metadata(&oid_key(*oid))?;
        }
    }
    for name in before.keys() {
        if !after.contains_key(name) {
            catalog.delete_metadata(&key(name))?;
        }
    }
    for (oid, name) in &owned_after {
        if owned_before.get(oid) != Some(name) {
            catalog.set_metadata(&oid_key(*oid), name)?;
        }
    }
    for (name, definition) in after {
        let json = serde_json::to_string(definition)?;
        if before
            .get(name)
            .map(serde_json::to_string)
            .transpose()?
            .as_ref()
            != Some(&json)
        {
            catalog.set_metadata(&key(name), &json)?;
        }
    }
    Ok(())
}
