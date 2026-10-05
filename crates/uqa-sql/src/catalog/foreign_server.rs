//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-server catalog identity and ownership, independent of a transport implementation.

use super::roles::{identity::RoleSubject, RoleDefinition, RoleIdentity};
use crate::SQLError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForeignServerMetadata {
    pub oid: u32,
    pub object_id: [u8; 16],
    pub owner: RoleIdentity,
    pub server_type: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignServerDefinition {
    pub name: String,
    pub fdw_type: String,
    pub options: BTreeMap<String, String>,
    pub metadata: ForeignServerMetadata,
}

/// Validate identities before publishing any restored definition. A name, OID or incarnation cannot alias another server, and an owner must still be the role incarnation that created it.
pub fn validate_foreign_servers(
    servers: &BTreeMap<String, ForeignServerDefinition>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    let mut oids = BTreeSet::new();
    let mut identities = BTreeSet::new();
    for (name, server) in servers {
        let metadata = &server.metadata;
        validate_foreign_server_name(name)?;
        if name != &server.name {
            return Err(invalid(name, "invalid catalog name"));
        }
        if metadata.oid < super::oids::FIRST_NORMAL_OBJECT_ID || !oids.insert(metadata.oid) {
            return Err(invalid(name, "invalid or duplicate catalog OID"));
        }
        if metadata.object_id == [0; 16] || !identities.insert(metadata.object_id) {
            return Err(invalid(name, "invalid or duplicate object identity"));
        }
        if !metadata.owner.is_valid() || metadata.owner.role_definition(roles).is_none() {
            return Err(invalid(name, "owner references a missing or replaced role"));
        }
    }
    Ok(())
}

/// SQL rejects an empty identifier during parsing; direct callers must reject it before a durable definition is written.
pub fn validate_foreign_server_name(name: &str) -> Result<(), SQLError> {
    if name.is_empty() {
        return Err(invalid(name, "invalid catalog name"));
    }
    Ok(())
}

fn invalid(name: &str, reason: &str) -> SQLError {
    SQLError::Internal(format!("foreign server `{name}`: {reason}"))
}

#[cfg(test)]
mod tests;
