//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate table and column ACL grant paths against durable relation metadata.
use super::{RoleDefinition, TableAclEntry, TableAclPrivilege, TableSecurity};
use crate::catalog::security::columns::column_grant_option_roles;
use std::collections::{BTreeMap, BTreeSet};

pub fn validate_table_security_invariants(
    security: &TableSecurity,
    columns: Option<&[String]>,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), String> {
    let validate_acl = |acl: &[TableAclEntry], column: Option<&str>| -> Result<(), String> {
        let mut paths = BTreeSet::new();
        for entry in acl {
            let grantor = super::acl_grantor(entry, &security.role_owner);
            if entry.role != "PUBLIC" && !roles.contains_key(&entry.role) {
                return Err(format!(
                    "ACL references missing grantee role `{}`",
                    entry.role
                ));
            }
            if grantor == "PUBLIC" || !roles.contains_key(grantor) {
                return Err(format!("ACL references missing grantor role `{grantor}`"));
            }
            if !paths.insert((entry.role.as_str(), grantor)) {
                return Err(format!(
                    "ACL contains duplicate grant path `{grantor}` -> `{}`",
                    entry.role
                ));
            }
            if entry.privileges.is_empty() && entry.grant_options.is_empty() {
                return Err("ACL contains an empty grant path".into());
            }
            if entry.role == "PUBLIC" && !entry.grant_options.is_empty() {
                return Err("PUBLIC cannot hold grant options".into());
            }
            for privilege in TableAclPrivilege::ALL {
                let mask = privilege.mask();
                if entry.grant_options.intersects(mask) && !entry.privileges.intersects(mask) {
                    return Err("ACL grant option exists without its privilege".into());
                }
                if entry.privileges.intersects(mask) || entry.grant_options.intersects(mask) {
                    let reachable = column.map_or_else(
                        || super::grant_option_roles(security, privilege),
                        |column| column_grant_option_roles(security, column, privilege),
                    );
                    if !reachable.contains(grantor) {
                        return Err(format!(
                            "ACL grant path from `{grantor}` is not rooted at owner `{}`",
                            security.role_owner
                        ));
                    }
                }
            }
        }
        Ok(())
    };

    if let Some(acl) = security.acl.as_deref() {
        validate_acl(acl, None)?;
    }
    if !security.column_acls.is_empty() && columns.is_none() {
        return Err("column ACLs require durable public column metadata".into());
    }
    for (column, acl) in &security.column_acls {
        if !columns.is_some_and(|columns| columns.iter().any(|candidate| candidate == column)) {
            return Err(format!("column ACL references missing column `{column}`"));
        }
        for entry in acl {
            if entry.privileges.delete
                || entry.privileges.truncate
                || entry.privileges.trigger
                || entry.privileges.maintain
                || entry.grant_options.delete
                || entry.grant_options.truncate
                || entry.grant_options.trigger
                || entry.grant_options.maintain
            {
                return Err(format!(
                    "column ACL for `{column}` contains a relation-only privilege"
                ));
            }
        }
        validate_acl(acl, Some(column))?;
    }
    Ok(())
}
