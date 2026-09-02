//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary-table ACL privilege sets, grant paths, and dependency-aware revocation.

use std::collections::{BTreeMap, BTreeSet};

use uqa_sql::ast::{RoleAttribute, TablePrivilege, TablePrivilegeSpec};
use uqa_sql::SQLError;
use uqa_storage::{TableAclEntry, TablePrivileges};

use crate::engine_roles::{role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey};
use crate::engine_state::TableSecurity;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TableAclPrivilege {
    Select,
    Insert,
    Update,
    Delete,
    Truncate,
    References,
    Trigger,
    Maintain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TablePrivilegeCheck {
    pub(super) privilege: TableAclPrivilege,
    pub(super) grant_option: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RequestedTablePrivileges {
    pub(super) table: Vec<TableAclPrivilege>,
    pub(super) columns: Vec<(TableAclPrivilege, String)>,
}

impl TableAclPrivilege {
    pub(super) const ALL: [Self; 8] = [
        Self::Select,
        Self::Insert,
        Self::Update,
        Self::Delete,
        Self::Truncate,
        Self::References,
        Self::Trigger,
        Self::Maintain,
    ];

    pub(crate) const COLUMN_ALL: [Self; 4] =
        [Self::Select, Self::Insert, Self::Update, Self::References];

    pub(super) const fn mask(self) -> TablePrivileges {
        let mut privileges = TablePrivileges {
            select: false,
            insert: false,
            update: false,
            delete: false,
            truncate: false,
            references: false,
            trigger: false,
            maintain: false,
        };
        match self {
            Self::Select => privileges.select = true,
            Self::Insert => privileges.insert = true,
            Self::Update => privileges.update = true,
            Self::Delete => privileges.delete = true,
            Self::Truncate => privileges.truncate = true,
            Self::References => privileges.references = true,
            Self::Trigger => privileges.trigger = true,
            Self::Maintain => privileges.maintain = true,
        }
        privileges
    }
}

pub(super) fn requested_acl_privileges(
    requested: &[TablePrivilegeSpec],
) -> Result<RequestedTablePrivileges, SQLError> {
    if requested.is_empty() {
        return Ok(RequestedTablePrivileges {
            table: TableAclPrivilege::ALL.into(),
            columns: Vec::new(),
        });
    }
    let mut table = Vec::with_capacity(requested.len());
    let mut columns = Vec::new();
    for spec in requested {
        let privilege = match &spec.privilege {
            TablePrivilege::Select => TableAclPrivilege::Select,
            TablePrivilege::Insert => TableAclPrivilege::Insert,
            TablePrivilege::Update => TableAclPrivilege::Update,
            TablePrivilege::Delete => TableAclPrivilege::Delete,
            TablePrivilege::Truncate => TableAclPrivilege::Truncate,
            TablePrivilege::References => TableAclPrivilege::References,
            TablePrivilege::Trigger => TableAclPrivilege::Trigger,
            TablePrivilege::Maintain => TableAclPrivilege::Maintain,
            TablePrivilege::Usage => {
                return Err(SQLError::Routine {
                    sqlstate: "0LP01".into(),
                    message: if spec.columns.is_empty() {
                        "invalid privilege type USAGE for table".into()
                    } else {
                        "invalid privilege type USAGE for column".into()
                    },
                })
            }
            TablePrivilege::Unsupported(name) => {
                return Err(SQLError::Routine {
                    sqlstate: "0LP01".into(),
                    message: format!(
                        "invalid privilege type {name} for {}",
                        if spec.columns.is_empty() {
                            "table"
                        } else {
                            "column"
                        }
                    ),
                })
            }
        };
        if spec.columns.is_empty() {
            if !table.contains(&privilege) {
                table.push(privilege);
            }
        } else {
            if !TableAclPrivilege::COLUMN_ALL.contains(&privilege) {
                return Err(SQLError::Routine {
                    sqlstate: "0LP01".into(),
                    message: format!(
                        "invalid privilege type {} for column",
                        table_privilege_name(&spec.privilege)
                    ),
                });
            }
            for column in &spec.columns {
                let requested = (privilege, column.clone());
                if !columns.contains(&requested) {
                    columns.push(requested);
                }
            }
        }
    }
    Ok(RequestedTablePrivileges { table, columns })
}

fn table_privilege_name(privilege: &TablePrivilege) -> &str {
    match privilege {
        TablePrivilege::Select => "SELECT",
        TablePrivilege::Insert => "INSERT",
        TablePrivilege::Update => "UPDATE",
        TablePrivilege::Delete => "DELETE",
        TablePrivilege::Truncate => "TRUNCATE",
        TablePrivilege::References => "REFERENCES",
        TablePrivilege::Trigger => "TRIGGER",
        TablePrivilege::Maintain => "MAINTAIN",
        TablePrivilege::Usage => "USAGE",
        TablePrivilege::Unsupported(name) => name,
    }
}

pub(super) fn parse_privilege_checks(value: &str) -> Result<Vec<TablePrivilegeCheck>, SQLError> {
    value
        .split(',')
        .map(|item| {
            let item = item.trim();
            let upper = item.to_ascii_uppercase();
            let (name, grant_option) = upper
                .strip_suffix(" WITH GRANT OPTION")
                .map_or((upper.as_str(), false), |name| (name.trim_end(), true));
            let privilege = match name {
                "SELECT" => TableAclPrivilege::Select,
                "INSERT" => TableAclPrivilege::Insert,
                "UPDATE" => TableAclPrivilege::Update,
                "DELETE" => TableAclPrivilege::Delete,
                "TRUNCATE" => TableAclPrivilege::Truncate,
                "REFERENCES" => TableAclPrivilege::References,
                "TRIGGER" => TableAclPrivilege::Trigger,
                "MAINTAIN" => TableAclPrivilege::Maintain,
                _ => {
                    return Err(SQLError::Routine {
                        sqlstate: "22023".into(),
                        message: format!("unrecognized privilege type: \"{item}\""),
                    })
                }
            };
            Ok(TablePrivilegeCheck {
                privilege,
                grant_option,
            })
        })
        .collect()
}

pub(super) fn parse_column_privilege_checks(
    value: &str,
) -> Result<Vec<TablePrivilegeCheck>, SQLError> {
    let checks = parse_privilege_checks(value)?;
    if let Some(invalid) = checks
        .iter()
        .find(|check| !TableAclPrivilege::COLUMN_ALL.contains(&check.privilege))
    {
        let item = value
            .split(',')
            .find(|item| {
                let upper = item.trim().to_ascii_uppercase();
                let name = upper
                    .strip_suffix(" WITH GRANT OPTION")
                    .unwrap_or(&upper)
                    .trim_end();
                table_privilege_name_from_acl(invalid.privilege) == name
            })
            .map_or(value, str::trim);
        return Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!("unrecognized privilege type: \"{item}\""),
        });
    }
    Ok(checks)
}

fn table_privilege_name_from_acl(privilege: TableAclPrivilege) -> &'static str {
    match privilege {
        TableAclPrivilege::Select => "SELECT",
        TableAclPrivilege::Insert => "INSERT",
        TableAclPrivilege::Update => "UPDATE",
        TableAclPrivilege::Delete => "DELETE",
        TableAclPrivilege::Truncate => "TRUNCATE",
        TableAclPrivilege::References => "REFERENCES",
        TableAclPrivilege::Trigger => "TRIGGER",
        TableAclPrivilege::Maintain => "MAINTAIN",
    }
}

pub(super) fn acl_grantor<'a>(entry: &'a TableAclEntry, owner: &'a str) -> &'a str {
    entry.grantor.as_deref().unwrap_or(owner)
}

fn materialize_acl(security: &mut TableSecurity) {
    if security.acl.is_none() {
        security.acl = Some(vec![TableAclEntry {
            role: security.role_owner.clone(),
            grantor: Some(security.role_owner.clone()),
            privileges: TablePrivileges::ALL,
            grant_options: TablePrivileges::default(),
        }]);
    }
}

pub(super) fn grant_option_roles(
    security: &TableSecurity,
    privilege: TableAclPrivilege,
) -> BTreeSet<String> {
    let mut reachable = BTreeSet::from([security.role_owner.clone()]);
    let Some(acl) = security.acl.as_ref() else {
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

pub(super) fn select_acl_grantor(
    security: &TableSecurity,
    privilege: TableAclPrivilege,
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> Option<String> {
    if role_inherits(roles, memberships, current_user, &security.role_owner) {
        return Some(security.role_owner.clone());
    }
    let grant_options = grant_option_roles(security, privilege);
    if grant_options.contains(current_user) {
        return Some(current_user.to_string());
    }
    security.acl.as_ref().and_then(|acl| {
        acl.iter()
            .filter(|entry| entry.role != "PUBLIC" && grant_options.contains(&entry.role))
            .find(|entry| role_inherits(roles, memberships, current_user, &entry.role))
            .map(|entry| entry.role.clone())
    })
}

pub(super) fn role_has_privilege(
    security: &TableSecurity,
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
        return grant_option_roles(security, check.privilege)
            .iter()
            .any(|role| role_inherits(roles, memberships, subject, role));
    }
    match security.acl.as_ref() {
        None => false,
        Some(acl) => acl.iter().any(|entry| {
            entry.privileges.intersects(check.privilege.mask())
                && (entry.role == "PUBLIC"
                    || role_inherits(roles, memberships, subject, &entry.role))
        }),
    }
}

pub(super) fn grant_acl(
    security: &mut TableSecurity,
    privilege: TableAclPrivilege,
    grantees: &[String],
    grantor: &str,
    grant_option: bool,
) {
    materialize_acl(security);
    let owner = security.role_owner.clone();
    let acl = security.acl.as_mut().expect("table ACL was materialized");
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

pub(super) fn revoke_acl(
    security: &mut TableSecurity,
    privilege: TableAclPrivilege,
    grantees: &[String],
    grantor: &str,
    grant_option_only: bool,
    cascade: bool,
) -> Result<(), SQLError> {
    let before = grant_option_roles(security, privilege);
    materialize_acl(security);
    let owner = security.role_owner.clone();
    let acl = security.acl.as_mut().expect("table ACL was materialized");
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
    revoke_dependent_acl(security, privilege, &before, cascade)
}

fn revoke_dependent_acl(
    security: &mut TableSecurity,
    privilege: TableAclPrivilege,
    before: &BTreeSet<String>,
    cascade: bool,
) -> Result<(), SQLError> {
    loop {
        let current = grant_option_roles(security, privilege);
        let lost = before
            .difference(&current)
            .cloned()
            .collect::<BTreeSet<_>>();
        if lost.is_empty() {
            return Ok(());
        }
        let owner = security.role_owner.clone();
        let dependent = security.acl.as_ref().is_some_and(|acl| {
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
            .acl
            .as_mut()
            .expect("dependent table privileges require an explicit ACL");
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

pub(super) fn rewrite_acl_owner(security: &mut TableSecurity, new_owner: &str) {
    let old_owner = std::mem::replace(&mut security.role_owner, new_owner.to_string());
    if let Some(acl) = security.acl.as_mut() {
        rewrite_acl_entries_owner(acl, &old_owner, new_owner);
    }
    for column_acl in security.column_acls.values_mut() {
        rewrite_acl_entries_owner(column_acl, &old_owner, new_owner);
    }
}

fn rewrite_acl_entries_owner(acl: &mut Vec<TableAclEntry>, old_owner: &str, new_owner: &str) {
    for entry in acl.iter_mut() {
        if entry.role == old_owner {
            entry.role = new_owner.to_string();
        }
        if entry.grantor.as_deref() == Some(old_owner) {
            entry.grantor = Some(new_owner.to_string());
        }
    }
    let mut merged: Vec<TableAclEntry> = Vec::with_capacity(acl.len());
    for entry in std::mem::take(acl) {
        if let Some(existing) = merged.iter_mut().find(|existing| {
            existing.role == entry.role
                && acl_grantor(existing, new_owner) == acl_grantor(&entry, new_owner)
        }) {
            existing.privileges.insert(entry.privileges);
            existing.grant_options.insert(entry.grant_options);
        } else {
            merged.push(entry);
        }
    }
    *acl = merged;
}
