//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable user-defined type catalogs: one metadata record per definition, keyed by qualified name, and one claim for every OID the definition owns. Claims are keyed by OID, so a concurrently committed claim for the same OID conflicts instead of aliasing another type.

use serde::{de::DeserializeOwned, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

/// The metadata keys and format marker of one type form's catalog.
pub(crate) struct TypeRecordFormat {
    /// The type form in diagnostics, such as `enum`.
    pub label: &'static str,
    /// The storage error context of a definition conflict, such as `enum definition`.
    pub definition_context: &'static str,
    /// The storage error context of an OID claim conflict, such as `enum OID`.
    pub oid_context: &'static str,
    pub format_key: &'static str,
    /// The marker field that carries the format version.
    pub format_field: &'static str,
    pub version: u64,
    pub prefix: &'static str,
    pub oid_prefix: &'static str,
}

/// A durable definition and the OIDs it claims.
pub(crate) trait TypeRecord: Serialize + DeserializeOwned + Clone {
    fn claimed_oids(&self) -> BTreeSet<u32>;
}

impl TypeRecordFormat {
    pub(crate) fn key(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }

    fn oid_key(&self, oid: u32) -> String {
        format!("{}{oid}", self.oid_prefix)
    }

    fn marker(&self) -> String {
        format!("{{\"{}\":{}}}", self.format_field, self.version)
    }

    fn corrupt(message: impl Into<String>) -> StorageBackendError {
        StorageBackendError::Other(message.into())
    }

    fn check_marker(&self, marker: &str) -> StorageBackendResult<()> {
        let marker: serde_json::Map<String, serde_json::Value> = serde_json::from_str(marker)?;
        let version = match (marker.len(), marker.get(self.format_field)) {
            (1, Some(serde_json::Value::Number(version))) => version.as_u64(),
            _ => None,
        }
        .ok_or_else(|| Self::corrupt(format!("malformed {} catalog format marker", self.label)))?;
        if version != self.version {
            return Err(Self::corrupt(format!(
                "unsupported {} catalog format {version}",
                self.label
            )));
        }
        Ok(())
    }

    /// Read every definition and check that the claims name exactly the OIDs the definitions own.
    pub(crate) fn read<T: TypeRecord>(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<BTreeMap<String, T>> {
        let records = catalog.metadata_with_prefix(self.prefix)?;
        let claims = catalog.metadata_with_prefix(self.oid_prefix)?;
        match catalog.get_metadata(self.format_key)? {
            Some(marker) => self.check_marker(&marker)?,
            None if records.is_empty() && claims.is_empty() => return Ok(BTreeMap::new()),
            None => {
                return Err(Self::corrupt(format!(
                    "{} records exist without their format marker",
                    self.label
                )))
            }
        }
        let mut registry = BTreeMap::new();
        for (record, json) in records {
            let name = record
                .strip_prefix(self.prefix)
                .ok_or_else(|| Self::corrupt(format!("{} record key {record}", self.label)))?;
            registry.insert(name.to_owned(), serde_json::from_str(&json)?);
        }
        let expected = registry
            .iter()
            .flat_map(|(name, definition): (&String, &T)| {
                definition
                    .claimed_oids()
                    .into_iter()
                    .map(move |oid| (self.oid_key(oid), name.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        if claims.into_iter().collect::<BTreeMap<_, _>>() != expected {
            return Err(Self::corrupt(format!(
                "{} OID records do not match {} definitions",
                self.label, self.label
            )));
        }
        Ok(registry)
    }

    /// Apply a statement's changes to the current registry. A definition changed by another writer since the statement read it is a serialization failure rather than a silent overwrite.
    pub(crate) fn merge_changes<T: TypeRecord>(
        &self,
        before: &BTreeMap<String, T>,
        after: &BTreeMap<String, T>,
        current: &BTreeMap<String, T>,
    ) -> StorageBackendResult<BTreeMap<String, T>> {
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
                    self.definition_context,
                    uqa_sql::SQLError::Routine {
                        sqlstate: "40001".into(),
                        message: format!(
                            "{} type `{name}` changed during catalog publication",
                            self.label
                        ),
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

    /// Write changed definitions and their claims in the caller's transaction.
    pub(crate) fn persist<T: TypeRecord>(
        &self,
        catalog: &dyn CatalogFacade,
        before: &BTreeMap<String, T>,
        after: &BTreeMap<String, T>,
    ) -> StorageBackendResult<()> {
        if !after.is_empty() && catalog.get_metadata(self.format_key)?.is_none() {
            catalog.set_metadata(self.format_key, &self.marker())?;
        }
        let owned = |registry: &BTreeMap<String, T>| {
            registry
                .iter()
                .flat_map(|(name, definition)| {
                    definition
                        .claimed_oids()
                        .into_iter()
                        .map(move |oid| (oid, name.clone()))
                })
                .collect::<BTreeMap<_, _>>()
        };
        let owned_before = owned(before);
        let owned_after = owned(after);
        for (oid, name) in &owned_after {
            if owned_before.get(oid) == Some(name) {
                continue;
            }
            if let Some(claimed) = catalog.get_metadata(&self.oid_key(*oid))? {
                if owned_before.get(oid) != Some(&claimed) {
                    return Err(StorageBackendError::backend(
                        self.oid_context,
                        uqa_sql::SQLError::Routine {
                            sqlstate: "23505".into(),
                            message: format!("{} OID {oid} is already assigned", self.label),
                        },
                    ));
                }
            }
        }
        for (oid, name) in &owned_before {
            if owned_after.get(oid) != Some(name) {
                catalog.delete_metadata(&self.oid_key(*oid))?;
            }
        }
        for name in before.keys() {
            if !after.contains_key(name) {
                catalog.delete_metadata(&self.key(name))?;
            }
        }
        for (oid, name) in &owned_after {
            if owned_before.get(oid) != Some(name) {
                catalog.set_metadata(&self.oid_key(*oid), name)?;
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
                catalog.set_metadata(&self.key(name), &json)?;
            }
        }
        Ok(())
    }
}
