//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently persisted ACL tuples for immutable builtin routine identities.

use std::{collections::BTreeMap, ops::DerefMut};
use uqa_sql::catalog::{
    roles::RoleDefinition,
    security::builtin_routines::{
        metadata_key, validate, BuiltinRoutineSecurities, BuiltinRoutineSecurity,
        BuiltinRoutineSecurityCatalog, METADATA_PREFIX,
    },
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub trait BuiltinRoutineSecurityState: BuiltinRoutineSecurityCatalog {
    fn builtin_routine_securities_write(
        &self,
    ) -> Box<dyn DerefMut<Target = BuiltinRoutineSecurities> + '_>;
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSecurity {
    builtin_routine_acl_format: u32,
    entry: BuiltinRoutineSecurity,
}

fn encode(entry: &BuiltinRoutineSecurity) -> StorageBackendResult<String> {
    Ok(serde_json::to_string(&StoredSecurity {
        builtin_routine_acl_format: 1,
        entry: entry.clone(),
    })?)
}

fn decode(
    json: &str,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<BuiltinRoutineSecurity> {
    let stored: StoredSecurity = serde_json::from_str(json)?;
    if stored.builtin_routine_acl_format != 1 {
        return Err(StorageBackendError::Other(format!(
            "unknown builtin routine ACL format {}",
            stored.builtin_routine_acl_format,
        )));
    }
    validate(&stored.entry, roles)
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    Ok(stored.entry)
}

pub fn restore(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<BuiltinRoutineSecurities> {
    let mut result = BuiltinRoutineSecurities::new();
    for (key, json) in catalog.metadata_with_prefix(METADATA_PREFIX)? {
        let oid = key
            .strip_prefix(METADATA_PREFIX)
            .and_then(|text| text.parse::<u32>().ok())
            .filter(|oid| key == metadata_key(*oid))
            .ok_or_else(|| StorageBackendError::Other("invalid builtin routine ACL key".into()))?;
        if super::super::projection::builtin_routine_identity(oid).is_none() {
            return Err(StorageBackendError::Other(format!(
                "builtin routine ACL references missing function {oid}"
            )));
        }
        let entry = decode(&json, roles)?;
        if result.insert(oid, entry).is_some() {
            return Err(StorageBackendError::Other(
                "duplicate builtin routine ACL record".into(),
            ));
        }
    }
    Ok(result)
}

/// A writer publishes all candidates only after every durable write succeeds.
pub struct BuiltinRoutinePrivilegeUpdate {
    pub oid: u32,
    pub entry: BuiltinRoutineSecurity,
}
impl BuiltinRoutinePrivilegeUpdate {
    pub fn new(
        oid: u32,
        execute_acl: Vec<uqa_sql::ast::RoutineAclEntry>,
    ) -> StorageBackendResult<Self> {
        let revision = crate::catalog::identity::new_nonzero_catalog_identity(
            &oid.to_string(),
            "builtin routine ACL tuple",
        )?;
        Ok(Self {
            oid,
            entry: BuiltinRoutineSecurity {
                revision,
                execute_acl,
            },
        })
    }
    pub fn persist(&self, catalog: Option<&dyn CatalogFacade>) -> StorageBackendResult<()> {
        if let Some(catalog) = catalog {
            catalog.set_metadata(&metadata_key(self.oid), &encode(&self.entry)?)?;
        }
        Ok(())
    }
}

/// Each changed ACL is an independent metadata record; a private mutation of one
/// builtin does not hide a newly committed privilege change to another.
pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &BuiltinRoutineSecurities,
    mut latest: BuiltinRoutineSecurities,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<BuiltinRoutineSecurities> {
    if let Some(catalog) = catalog {
        for (oid, entry) in current {
            if catalog.metadata_has_private_changes(&metadata_key(*oid))? {
                latest.insert(*oid, entry.clone());
            }
        }
    }
    for entry in latest.values() {
        validate(entry, roles).map_err(|error| StorageBackendError::Other(error.to_string()))?;
    }
    Ok(latest)
}

pub mod execution;

#[cfg(test)]
mod tests;
