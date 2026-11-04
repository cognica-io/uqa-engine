//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned database security stores bound role references and converts legacy names once.

use serde::{Deserialize, Serialize};
use uqa_sql::catalog::roles::RoleDefinitions;
use uqa_sql::catalog::security::database::{
    binding::BoundDatabaseSecurity, DatabaseAuthority, DatabaseSecurity,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

use super::DATABASE_SECURITY_METADATA_KEY;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDatabaseSecurity<T> {
    database_security_format: u32,
    security: T,
}

pub(super) fn encode(
    security: &BoundDatabaseSecurity,
    roles: &RoleDefinitions,
) -> StorageBackendResult<String> {
    security
        .validate(roles)
        .map_err(StorageBackendError::Other)?;
    Ok(serde_json::to_string(&StoredDatabaseSecurity {
        database_security_format: 1,
        security,
    })?)
}

pub(super) fn restore(
    catalog: &dyn CatalogFacade,
    roles: &RoleDefinitions,
    allow_migration: bool,
    current: &DatabaseAuthority,
) -> StorageBackendResult<DatabaseAuthority> {
    let json = catalog.get_metadata(DATABASE_SECURITY_METADATA_KEY)?;
    let value = json
        .as_deref()
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()?;
    if value
        .as_ref()
        .is_some_and(|value| value.get("database_security_format").is_some())
    {
        let stored: StoredDatabaseSecurity<BoundDatabaseSecurity> =
            serde_json::from_value(value.expect("versioned database security exists"))?;
        if stored.database_security_format != 1 {
            return Err(StorageBackendError::Other(format!(
                "unsupported database security format {}",
                stored.database_security_format
            )));
        }
        stored
            .security
            .validate(roles)
            .map_err(StorageBackendError::Other)?;
        return current.successor(&stored.security).map_err(|error| {
            StorageBackendError::backend("database authority restoration", error)
        });
    }
    if !allow_migration {
        return Err(StorageBackendError::Other(
            "database security requires initial catalog migration".into(),
        ));
    }
    let bound = if let Some(value) = value {
        let legacy: DatabaseSecurity = serde_json::from_value(value)?;
        BoundDatabaseSecurity::bind(&legacy, roles).map_err(StorageBackendError::Other)?
    } else {
        BoundDatabaseSecurity::bootstrap()
    };
    let next = current
        .successor(&bound)
        .map_err(|error| StorageBackendError::backend("database authority restoration", error))?;
    catalog.set_metadata(DATABASE_SECURITY_METADATA_KEY, &encode(&next, roles)?)?;
    Ok(next)
}

#[cfg(test)]
mod tests;
