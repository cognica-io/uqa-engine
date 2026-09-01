//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence ACL privilege sets, grant paths, and dependency-aware revocation.

use std::collections::{BTreeMap, BTreeSet};

use uqa_sql::ast::{RoleAttribute, SequencePrivilege};
use uqa_sql::SQLError;
use uqa_storage::{SequenceAclEntry, SequencePrivileges};

use crate::engine_roles::{role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey};
use crate::engine_state::SequenceSecurity;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum AclPrivilege {
    Select,
    Update,
    Usage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PrivilegeCheck {
    pub(super) privilege: AclPrivilege,
    pub(super) grant_option: bool,
}

impl AclPrivilege {
    pub(super) const fn mask(self) -> SequencePrivileges {
        match self {
            Self::Select => SequencePrivileges {
                select: true,
                update: false,
                usage: false,
            },
            Self::Update => SequencePrivileges {
                select: false,
                update: true,
                usage: false,
            },
            Self::Usage => SequencePrivileges {
                select: false,
                update: false,
                usage: true,
            },
        }
    }
}

pub(super) fn requested_acl_privileges(
    requested: &[SequencePrivilege],
) -> Result<Vec<AclPrivilege>, SQLError> {
    requested
        .iter()
        .map(|privilege| match privilege {
            SequencePrivilege::Select => Ok(AclPrivilege::Select),
            SequencePrivilege::Update => Ok(AclPrivilege::Update),
            SequencePrivilege::Usage => Ok(AclPrivilege::Usage),
            SequencePrivilege::ColumnsUnsupported => Err(SQLError::Routine {
                sqlstate: "0LP01".into(),
                message: "column privileges are only valid for relations".into(),
            }),
            SequencePrivilege::Unsupported(name) => Err(SQLError::Routine {
                sqlstate: "0LP01".into(),
                message: format!("invalid privilege type {name} for sequence"),
            }),
        })
        .collect()
}

pub(super) fn parse_privilege_checks(value: &str) -> Result<Vec<PrivilegeCheck>, SQLError> {
    value
        .split(',')
        .map(|item| {
            let item = item.trim();
            let upper = item.to_ascii_uppercase();
            let (name, grant_option) = upper
                .strip_suffix(" WITH GRANT OPTION")
                .map_or((upper.as_str(), false), |name| (name.trim_end(), true));
            let privilege = match name {
                "SELECT" => AclPrivilege::Select,
                "UPDATE" => AclPrivilege::Update,
                "USAGE" => AclPrivilege::Usage,
                _ => {
                    return Err(SQLError::Routine {
                        sqlstate: "22023".into(),
                        message: format!("unrecognized privilege type: \"{item}\""),
                    })
                }
            };
            Ok(PrivilegeCheck {
                privilege,
                grant_option,
            })
        })
        .collect()
}

fn acl_grantor<'a>(entry: &'a SequenceAclEntry, owner: &'a str) -> &'a str {
    entry.grantor.as_deref().unwrap_or(owner)
}

fn materialize_acl(security: &mut SequenceSecurity) {
    if security.acl.is_none() {
        security.acl = Some(vec![SequenceAclEntry {
            role: security.role_owner.clone(),
            grantor: Some(security.role_owner.clone()),
            privileges: SequencePrivileges::ALL,
            grant_options: SequencePrivileges::default(),
        }]);
    }
}

fn grant_option_roles(security: &SequenceSecurity, privilege: AclPrivilege) -> BTreeSet<String> {
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
    security: &SequenceSecurity,
    privilege: AclPrivilege,
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
    security: &SequenceSecurity,
    subject: &str,
    check: PrivilegeCheck,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> bool {
    if roles
        .get(subject)
        .is_some_and(|role| role.has(RoleAttribute::Superuser))
    {
        return true;
    }
    if check.grant_option {
        return grant_option_roles(security, check.privilege)
            .iter()
            .any(|role| role_inherits(roles, memberships, subject, role));
    }
    match security.acl.as_ref() {
        None => role_inherits(roles, memberships, subject, &security.role_owner),
        Some(acl) => acl.iter().any(|entry| {
            entry.privileges.intersects(check.privilege.mask())
                && (entry.role == "PUBLIC"
                    || role_inherits(roles, memberships, subject, &entry.role))
        }),
    }
}

pub(super) fn grant_acl(
    security: &mut SequenceSecurity,
    privilege: AclPrivilege,
    grantees: &[String],
    grantor: &str,
    grant_option: bool,
) {
    materialize_acl(security);
    let owner = security.role_owner.clone();
    let acl = security
        .acl
        .as_mut()
        .expect("sequence ACL was materialized");
    for grantee in grantees {
        let position = acl
            .iter()
            .position(|entry| entry.role == *grantee && acl_grantor(entry, &owner) == grantor)
            .unwrap_or_else(|| {
                acl.push(SequenceAclEntry {
                    role: grantee.clone(),
                    grantor: Some(grantor.to_string()),
                    privileges: SequencePrivileges::default(),
                    grant_options: SequencePrivileges::default(),
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
    security: &mut SequenceSecurity,
    privilege: AclPrivilege,
    grantees: &[String],
    grantor: &str,
    grant_option_only: bool,
    cascade: bool,
) -> Result<(), SQLError> {
    let before = grant_option_roles(security, privilege);
    materialize_acl(security);
    let owner = security.role_owner.clone();
    let acl = security
        .acl
        .as_mut()
        .expect("sequence ACL was materialized");
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
    security: &mut SequenceSecurity,
    privilege: AclPrivilege,
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
            .expect("dependent sequence privileges require an explicit ACL");
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

fn remove_empty_entries(acl: &mut Vec<SequenceAclEntry>) {
    acl.retain(|entry| !entry.privileges.is_empty() || !entry.grant_options.is_empty());
}

pub(super) fn rewrite_acl_owner(security: &mut SequenceSecurity, new_owner: &str) {
    let old_owner = std::mem::replace(&mut security.role_owner, new_owner.to_string());
    let Some(acl) = security.acl.as_mut() else {
        return;
    };
    for entry in acl.iter_mut() {
        if entry.role == old_owner {
            entry.role = new_owner.to_string();
        }
        if entry.grantor.as_deref() == Some(&old_owner) {
            entry.grantor = Some(new_owner.to_string());
        }
    }
    let mut merged: Vec<SequenceAclEntry> = Vec::with_capacity(acl.len());
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
