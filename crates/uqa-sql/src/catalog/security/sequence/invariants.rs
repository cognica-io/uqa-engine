//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate sequence ACL endpoints and grant paths before catalog publication or migration.

use super::{AclPrivilege, BTreeMap, BTreeSet, RoleDefinition, SequenceSecurity};

pub fn validate_sequence_security_invariants(
    security: &SequenceSecurity,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    if !roles.contains_key(&security.role_owner) {
        return Err(format!(
            "sequence references missing owner role `{}`",
            security.role_owner
        ));
    }
    let mut paths = BTreeSet::new();
    for entry in security.acl.iter().flatten() {
        let grantor = super::acl_grantor(entry, &security.role_owner);
        if entry
            .role
            .role_name()
            .is_some_and(|name| !roles.contains_key(name))
        {
            return Err(format!(
                "ACL references missing grantee role `{}`",
                entry.role
            ));
        }
        if !roles.contains_key(grantor) {
            return Err(format!("ACL references missing grantor role `{grantor}`"));
        }
        if !paths.insert((&entry.role, grantor)) {
            return Err(format!(
                "ACL contains duplicate grant path `{grantor}` -> `{}`",
                entry.role
            ));
        }
        if entry.privileges.is_empty() && entry.grant_options.is_empty() {
            return Err("ACL contains an empty grant path".into());
        }
        if entry.role.is_public() && !entry.grant_options.is_empty() {
            return Err("PUBLIC cannot hold grant options".into());
        }
        for privilege in [
            AclPrivilege::Select,
            AclPrivilege::Update,
            AclPrivilege::Usage,
        ] {
            let mask = privilege.mask();
            if entry.grant_options.intersects(mask) && !entry.privileges.intersects(mask) {
                return Err("ACL grant option exists without its privilege".into());
            }
            if (entry.privileges.intersects(mask) || entry.grant_options.intersects(mask))
                && !super::grant_option_roles(security, privilege).contains(grantor)
            {
                return Err(format!(
                    "ACL grant path from `{grantor}` is not rooted at owner `{}`",
                    security.role_owner
                ));
            }
        }
    }
    Ok(())
}
