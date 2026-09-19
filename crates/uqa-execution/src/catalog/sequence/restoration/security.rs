//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind legacy sequence ACL endpoints only during complete initial catalog restoration.

use std::collections::BTreeMap;
use uqa_sql::catalog::{
    roles::RoleDefinition,
    security::{BoundSequenceSecurity, SequenceSecurity},
};
use uqa_storage::{SequenceSecurityRow, StorageBackendError, StorageBackendResult};

pub(super) fn restore_security(
    row: &SequenceSecurityRow,
    roles: &BTreeMap<String, RoleDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<BoundSequenceSecurity> {
    let security = match row {
        SequenceSecurityRow::Bound(row) => BoundSequenceSecurity::from_row(row.clone()),
        SequenceSecurityRow::Legacy(row) => {
            if !allow_migration {
                return Err(StorageBackendError::Other(
                    "sequence security requires initial catalog migration".into(),
                ));
            }
            BoundSequenceSecurity::bind(
                &SequenceSecurity {
                    role_owner: row.role_owner.clone(),
                    acl: row.acl.clone(),
                },
                roles,
            )
            .map_err(StorageBackendError::Other)?
        }
    };
    security
        .validate(roles)
        .map_err(StorageBackendError::Other)?;
    Ok(security)
}
