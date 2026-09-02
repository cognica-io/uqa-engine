//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary-table column ACL grant paths and privilege checks.

use std::collections::{BTreeMap, BTreeSet};

use uqa_sql::ast::RoleAttribute;
use uqa_sql::SQLError;
use uqa_storage::{TableAclEntry, TablePrivileges};

use super::acl::{acl_grantor, grant_option_roles, TableAclPrivilege, TablePrivilegeCheck};
use crate::engine_roles::{role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey};
use crate::engine_state::TableSecurity;

fn column_grant_option_roles(
    security: &TableSecurity,
    column: &str,
    privilege: TableAclPrivilege,
) -> BTreeSet<String> {
    let mut reachable = grant_option_roles(security, privilege);
    let Some(acl) = security.column_acls.get(column) else {
        return reachable;
    };
    loop {
        let mut changed = false;
        for entry in acl {
            if entry.role != "PUBLIC"
                && entry.grant_options.intersects(privilege.mask())
                && reachable.contains(acl_grantor(entry, &security.role_owner))
            {
                changed |= reachable.insert(entry.role.clone());
            }
        }
        if !changed {
            return reachable;
        }
    }
}

pub(super) fn select_column_acl_grantor(
    security: &TableSecurity,
    column: &str,
    privilege: TableAclPrivilege,
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> Option<String> {
    if role_inherits(roles, memberships, current_user, &security.role_owner) {
        return Some(security.role_owner.clone());
    }
    let grant_options = column_grant_option_roles(security, column, privilege);
    if grant_options.contains(current_user) {
        return Some(current_user.to_string());
    }
    grant_options
        .into_iter()
        .filter(|role| role != "PUBLIC" && role != &security.role_owner)
        .find(|role| role_inherits(roles, memberships, current_user, role))
}

pub(super) fn role_has_column_privilege(
    security: &TableSecurity,
    column: &str,
    subject: &str,
    check: TablePrivilegeCheck,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> bool {
    if roles
        .get(subject)
        .is_some_and(|role| role.has(RoleAttribute::Superuser))
        || role_inherits(roles, memberships, subject, &security.role_owner)
    {
        return true;
    }
    if check.grant_option {
        return column_grant_option_roles(security, column, check.privilege)
            .iter()
            .any(|role| role_inherits(roles, memberships, subject, role));
    }
    if super::acl::role_has_privilege(security, subject, check, roles, memberships) {
        return true;
    }
    security.column_acls.get(column).is_some_and(|acl| {
        acl.iter().any(|entry| {
            entry.privileges.intersects(check.privilege.mask())
                && (entry.role == "PUBLIC"
                    || role_inherits(roles, memberships, subject, &entry.role))
        })
    })
}

pub(super) fn grant_column_acl(
    security: &mut TableSecurity,
    column: &str,
    privilege: TableAclPrivilege,
    grantees: &[String],
    grantor: &str,
    grant_option: bool,
) {
    let owner = security.role_owner.clone();
    let acl = security.column_acls.entry(column.to_string()).or_default();
    for grantee in grantees {
        let position = acl
            .iter()
            .position(|entry| entry.role == *grantee && acl_grantor(entry, &owner) == grantor)
            .unwrap_or_else(|| {
                acl.push(TableAclEntry {
                    role: grantee.clone(),
                    grantor: Some(grantor.to_string()),
                    privileges: TablePrivileges::default(),
                    grant_options: TablePrivileges::default(),
                });
                acl.len() - 1
            });
        let entry = &mut acl[position];
        entry.privileges.insert(privilege.mask());
        if grant_option && grantee != "PUBLIC" && grantee != &owner {
            entry.grant_options.insert(privilege.mask());
        }
    }
}

pub(super) fn revoke_column_acl(
    security: &mut TableSecurity,
    column: &str,
    privilege: TableAclPrivilege,
    grantees: &[String],
    grantor: &str,
    grant_option_only: bool,
    cascade: bool,
) -> Result<(), SQLError> {
    let before = column_grant_option_roles(security, column, privilege);
    let owner = security.role_owner.clone();
    let Some(acl) = security.column_acls.get_mut(column) else {
        return Ok(());
    };
    for entry in acl
        .iter_mut()
        .filter(|entry| grantees.contains(&entry.role) && acl_grantor(entry, &owner) == grantor)
    {
        entry.grant_options.remove(privilege.mask());
        if !grant_option_only {
            entry.privileges.remove(privilege.mask());
        }
    }
    remove_empty_entries(acl);
    revoke_dependent_column_acl(security, column, privilege, &before, cascade)
}

fn revoke_dependent_column_acl(
    security: &mut TableSecurity,
    column: &str,
    privilege: TableAclPrivilege,
    before: &BTreeSet<String>,
    cascade: bool,
) -> Result<(), SQLError> {
    loop {
        let current = column_grant_option_roles(security, column, privilege);
        let lost = before
            .difference(&current)
            .cloned()
            .collect::<BTreeSet<_>>();
        if lost.is_empty() {
            return Ok(());
        }
        let owner = security.role_owner.clone();
        let dependent = security.column_acls.get(column).is_some_and(|acl| {
            acl.iter().any(|entry| {
                lost.contains(acl_grantor(entry, &owner))
                    && (entry.privileges.intersects(privilege.mask())
                        || entry.grant_options.intersects(privilege.mask()))
            })
        });
        if !dependent {
            return Ok(());
        }
        if !cascade {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: "dependent privileges exist".into(),
            });
        }
        let acl = security
            .column_acls
            .get_mut(column)
            .expect("dependent column privileges require an explicit ACL");
        for entry in acl
            .iter_mut()
            .filter(|entry| lost.contains(acl_grantor(entry, &owner)))
        {
            entry.privileges.remove(privilege.mask());
            entry.grant_options.remove(privilege.mask());
        }
        remove_empty_entries(acl);
    }
}

fn remove_empty_entries(acl: &mut Vec<TableAclEntry>) {
    acl.retain(|entry| !entry.privileges.is_empty() || !entry.grant_options.is_empty());
}
