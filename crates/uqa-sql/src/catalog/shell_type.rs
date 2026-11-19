//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! An undefined type owns its name and OID before any value layout or array type exists. Completing it preserves that identity.

use serde::{Deserialize, Serialize};
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};

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

mod validation;
pub use validation::{
    validate_shell_registry, validate_shell_registry_from_input,
    validate_shell_registry_with_control,
};

#[cfg(test)]
mod tests {
    use crate::catalog::roles::{RoleDefinition, RoleDefinitions};
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn retained_shells_reject_aliasing_corrupt_names_and_retired_owners() {
        let roles =
            RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap();
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
        let decoded: BTreeMap<String, StoredShellType> = serde_json::from_str(&encoded).unwrap();
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
