//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An undefined type owns its name and OID before any value layout or array type exists. Completing it preserves that identity.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};

use super::roles::{identity::RoleSubject, RoleDefinition};
use crate::ast::ObjectAclEntry;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredShellType {
    pub object_id: [u8; 16],
    pub oid: u32,
    pub identity: RelationIdentity,
    pub owner: RoleIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_acl: Option<Vec<ObjectAclEntry>>,
}

/// Reject inconsistent names, duplicate identities and stale owner incarnations before publishing any restored shell.
pub fn validate_shell_registry(
    registry: &BTreeMap<String, StoredShellType>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    let mut identities = BTreeSet::new();
    let mut oids = BTreeSet::new();
    for (name, definition) in registry {
        if definition.object_id == [0; 16] || !identities.insert(definition.object_id) {
            return Err(format!("invalid or duplicate shell identity for `{name}`"));
        }
        if definition.oid < super::oids::FIRST_NORMAL_OBJECT_ID || !oids.insert(definition.oid) {
            return Err(format!(
                "invalid or duplicate shell OID {} for `{name}`",
                definition.oid
            ));
        }
        if definition.identity.schema.is_empty()
            || !super::array_type_names::valid_type_name(&definition.identity.name)
            || definition.identity.qualified_name() != *name
        {
            return Err(format!("inconsistent shell name for `{name}`"));
        }
        if !definition.owner.is_valid() || definition.owner.role_definition(roles).is_none() {
            return Err(format!(
                "shell `{name}` references missing role incarnation {}",
                definition.owner.oid
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_shells_reject_aliasing_corrupt_names_and_retired_owners() {
        let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
        let shell = StoredShellType {
            object_id: [1; 16],
            oid: 20_000,
            identity: RelationIdentity::new("public", "pending"),
            owner: RoleIdentity::BOOTSTRAP,
            usage_acl: None,
        };
        let registry = BTreeMap::from([("public.pending".into(), shell.clone())]);
        validate_shell_registry(&registry, &roles).unwrap();
        let encoded = serde_json::to_string(&registry).unwrap();
        let decoded = serde_json::from_str(&encoded).unwrap();
        validate_shell_registry(&decoded, &roles).unwrap();
        for corrupt in 0..5 {
            let mut changed = shell.clone();
            match corrupt {
                0 => changed.object_id = [0; 16],
                1 => changed.oid = 0,
                2 => changed.identity.name = "another".into(),
                3 => changed.identity.schema.clear(),
                _ => changed.owner.oid = 21_000,
            }
            assert!(validate_shell_registry(
                &BTreeMap::from([("public.pending".into(), changed)]),
                &roles
            )
            .is_err());
        }
        for duplicate_oid in [true, false] {
            let mut other = shell.clone();
            other.identity.name = "another".into();
            if duplicate_oid {
                other.object_id = [2; 16];
            } else {
                other.oid += 1;
            }
            let mut duplicate = registry.clone();
            duplicate.insert("public.another".into(), other);
            assert!(validate_shell_registry(&duplicate, &roles).is_err());
        }
    }
}
