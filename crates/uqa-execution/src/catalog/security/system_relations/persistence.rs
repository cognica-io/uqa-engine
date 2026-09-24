//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned system ACL tuples retain incarnations; only initial restoration binds legacy names.

use super::{RoleDefinition, SystemAcl};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uqa_sql::catalog::security::{BoundTableSecurity, TableAclEntry};
use uqa_storage::{StorageBackendError, StorageBackendResult};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSystemAcl<T> {
    system_acl_format: u32,
    entry: T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySystemAcl {
    revision: [u8; 16],
    acl: Vec<TableAclEntry>,
}

pub(super) fn encode(entry: &SystemAcl) -> StorageBackendResult<String> {
    Ok(serde_json::to_string(&StoredSystemAcl {
        system_acl_format: 1,
        entry,
    })?)
}

pub(super) fn decode(
    json: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<(SystemAcl, bool)> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    if value.get("system_acl_format").is_some() {
        let stored: StoredSystemAcl<SystemAcl> = serde_json::from_value(value)?;
        if stored.system_acl_format != 1 {
            return Err(StorageBackendError::Other(format!(
                "unsupported system ACL format {}",
                stored.system_acl_format
            )));
        }
        return Ok((stored.entry, false));
    }
    if !allow_migration {
        return Err(StorageBackendError::Other(
            "system ACL requires initial catalog migration".into(),
        ));
    }
    let legacy: LegacySystemAcl = serde_json::from_value(value)?;
    let mut security = BoundTableSecurity::owner(uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP)
        .resolve(roles)
        .map_err(StorageBackendError::Other)?;
    security.acl = Some(legacy.acl);
    let bound = BoundTableSecurity::bind(&security, roles).map_err(StorageBackendError::Other)?;
    Ok((
        SystemAcl {
            revision: legacy.revision,
            acl: bound.acl.expect("legacy system ACL is explicit"),
        },
        true,
    ))
}
