//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent role definitions and their OID uniqueness records.

use super::{
    BTreeMap, CatalogFacade, RoleDefinition, SQLError, StorageBackendError, StorageBackendResult,
    ROLES_METADATA_KEY,
};
use serde::Deserialize;

pub(super) const ROLE_PREFIX: &str = "uqa.sql.role.v1:";
const OID_PREFIX: &str = "uqa.sql.role_oid.v1:";
const FORMAT: &str = r#"{"role_catalog_format":3}"#;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RoleRecordFormat {
    Aggregate,
    Definitions,
    Identities,
    Revisions,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordFormat {
    role_catalog_format: u32,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredRoles {
    Records(RecordFormat),
    Legacy(BTreeMap<String, RoleDefinition>),
}

pub(super) fn role_key(name: &str) -> String {
    format!("{ROLE_PREFIX}{name}")
}

fn oid_key(oid: i64) -> String {
    format!("{OID_PREFIX}{oid}")
}

pub(super) fn read(
    catalog: &dyn CatalogFacade,
) -> StorageBackendResult<(BTreeMap<String, RoleDefinition>, RoleRecordFormat)> {
    let stored = catalog
        .get_metadata(ROLES_METADATA_KEY)?
        .map(|json| serde_json::from_str::<StoredRoles>(&json))
        .transpose()?;
    match stored {
        Some(StoredRoles::Records(format)) => {
            let format = match format.role_catalog_format {
                1 => RoleRecordFormat::Definitions,
                2 => RoleRecordFormat::Identities,
                3 => RoleRecordFormat::Revisions,
                version => {
                    return Err(StorageBackendError::Other(format!(
                        "unsupported role catalog format {version}"
                    )))
                }
            };
            let mut roles = BTreeMap::new();
            for (key, json) in catalog.metadata_with_prefix(ROLE_PREFIX)? {
                let name = key.strip_prefix(ROLE_PREFIX).expect("metadata prefix");
                roles.insert(name.to_owned(), serde_json::from_str(&json)?);
            }
            if !roles.values().any(|role: &RoleDefinition| role.oid == 10) {
                return Err(StorageBackendError::Other(
                    "role records are missing the bootstrap role".into(),
                ));
            }
            Ok((roles, format))
        }
        legacy => {
            if !catalog.metadata_with_prefix(ROLE_PREFIX)?.is_empty()
                || !catalog.metadata_with_prefix(OID_PREFIX)?.is_empty()
            {
                return Err(StorageBackendError::Other(
                    "role records exist without their format marker".into(),
                ));
            }
            Ok((
                match legacy {
                    Some(StoredRoles::Legacy(roles)) => roles,
                    None => BTreeMap::new(),
                    _ => unreachable!(),
                },
                RoleRecordFormat::Aggregate,
            ))
        }
    }
}

pub(super) fn validate_oids(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<()> {
    let claims = catalog.metadata_with_prefix(OID_PREFIX)?;
    if claims.len() != roles.len()
        || claims
            .iter()
            .any(|(key, name)| roles.get(name).is_none_or(|role| *key != oid_key(role.oid)))
    {
        return Err(StorageBackendError::Other(
            "role OID records do not match role definitions".into(),
        ));
    }
    Ok(())
}

/// Called only inside initial catalog restoration's owning transaction, after validating roles and memberships. The marker replaces the legacy map, so old readers fail to decode the new representation instead of silently using a stale aggregate.
pub(super) fn migrate(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
) -> StorageBackendResult<()> {
    for (name, role) in roles {
        catalog.set_metadata(&role_key(name), &serde_json::to_string(role)?)?;
        catalog.set_metadata(&oid_key(role.oid), name)?;
    }
    catalog.set_metadata(ROLES_METADATA_KEY, FORMAT)
}

pub(super) fn persist(
    catalog: &dyn CatalogFacade,
    before: &BTreeMap<String, RoleDefinition>,
    after: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    // OID records share the role publication transaction. Even creators that chose the same OID in different snapshots cannot both commit distinct owners of that OID.
    let mut assigned = std::collections::BTreeSet::new();
    for (name, role) in after {
        if !assigned.insert(role.oid)
            || (before.get(name).is_none_or(|old| old.oid != role.oid)
                && catalog
                    .get_metadata(&oid_key(role.oid))
                    .map_err(persist_error)?
                    .is_some_and(|claimed| {
                        before
                            .get(&claimed)
                            .is_none_or(|original| original.identity() != role.identity())
                    }))
        {
            return Err(SQLError::Routine {
                sqlstate: "23505".into(),
                message: format!("role OID {} is already assigned", role.oid),
            });
        }
    }
    uqa_sql::catalog::roles::restoration::validate_role_identities(after)
        .map_err(SQLError::Internal)?;
    uqa_sql::catalog::roles::tuple::validate_revision_changes(before, after)?;
    for (name, role) in before {
        if after.get(name).is_none_or(|next| next.oid != role.oid) {
            catalog
                .delete_metadata(&oid_key(role.oid))
                .map_err(persist_error)?;
        }
        if !after.contains_key(name) {
            catalog
                .delete_metadata(&role_key(name))
                .map_err(persist_error)?;
        }
    }
    for (name, role) in after {
        if before.get(name) == Some(role) {
            continue;
        }
        if before.get(name).is_none_or(|old| old.oid != role.oid) {
            catalog
                .set_metadata(&oid_key(role.oid), name)
                .map_err(persist_error)?;
        }
        let json = serde_json::to_string(role)
            .map_err(|error| SQLError::Internal(format!("serialize role catalog: {error}")))?;
        catalog
            .set_metadata(&role_key(name), &json)
            .map_err(persist_error)?;
    }
    Ok(())
}

fn persist_error(error: StorageBackendError) -> SQLError {
    SQLError::Internal(format!("persist role catalog: {error}"))
}
