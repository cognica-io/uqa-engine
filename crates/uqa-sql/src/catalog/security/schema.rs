//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema ACL privilege sets, grant paths, and dependency-aware revocation.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{GrantSchemaStmt, RoleAttribute, SchemaPrivilege, SchemaRevokeBehavior};
use crate::SQLError;
use uqa_core::catalog_schema::{SchemaAclEntry, SchemaPrivileges};

use super::SchemaSecurity;
use crate::catalog::roles::{role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchemaAclPrivilege {
    Usage,
    Create,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaPrivilegeCheck {
    pub privilege: SchemaAclPrivilege,
    pub grant_option: bool,
}

impl SchemaAclPrivilege {
    const fn mask(self) -> SchemaPrivileges {
        match self {
            Self::Usage => SchemaPrivileges {
                usage: true,
                create: false,
            },
            Self::Create => SchemaPrivileges {
                usage: false,
                create: true,
            },
        }
    }
}

pub fn requested_acl_privileges(
    requested: &[SchemaPrivilege],
) -> Result<Vec<SchemaAclPrivilege>, SQLError> {
    requested
        .iter()
        .map(|privilege| match privilege {
            SchemaPrivilege::Usage => Ok(SchemaAclPrivilege::Usage),
            SchemaPrivilege::Create => Ok(SchemaAclPrivilege::Create),
            SchemaPrivilege::Unsupported(name) => Err(SQLError::Routine {
                sqlstate: "0LP01".into(),
                message: format!("invalid privilege type {name} for schema"),
            }),
        })
        .collect()
}

pub fn parse_privilege_checks(value: &str) -> Result<Vec<SchemaPrivilegeCheck>, SQLError> {
    value
        .split(',')
        .map(|item| {
            let item = item.trim();
            let upper = item.to_ascii_uppercase();
            let (name, grant_option) = upper
                .strip_suffix(" WITH GRANT OPTION")
                .map_or((upper.as_str(), false), |name| (name.trim_end(), true));
            let privilege = match name {
                "USAGE" => SchemaAclPrivilege::Usage,
                "CREATE" => SchemaAclPrivilege::Create,
                _ => {
                    return Err(SQLError::Routine {
                        sqlstate: "22023".into(),
                        message: format!("unrecognized privilege type: \"{item}\""),
                    })
                }
            };
            Ok(SchemaPrivilegeCheck {
                privilege,
                grant_option,
            })
        })
        .collect()
}

fn acl_grantor<'a>(entry: &'a SchemaAclEntry, owner: &'a str) -> &'a str {
    entry.grantor.as_deref().unwrap_or(owner)
}

fn materialize_acl(security: &mut SchemaSecurity) {
    if security.acl.is_none() {
        security.acl = Some(vec![SchemaAclEntry {
            role: security.role_owner.clone(),
            grantor: Some(security.role_owner.clone()),
            privileges: SchemaPrivileges::ALL,
            grant_options: SchemaPrivileges::default(),
        }]);
    }
}

fn grant_option_roles(
    security: &SchemaSecurity,
    privilege: SchemaAclPrivilege,
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

pub fn select_acl_grantor(
    security: &SchemaSecurity,
    privilege: SchemaAclPrivilege,
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

pub fn role_has_schema_privilege(
    security: &SchemaSecurity,
    subject: &str,
    privilege: SchemaAclPrivilege,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> bool {
    role_has_schema_privilege_check(
        security,
        subject,
        SchemaPrivilegeCheck {
            privilege,
            grant_option: false,
        },
        roles,
        memberships,
    )
}

pub fn role_has_schema_privilege_check(
    security: &SchemaSecurity,
    subject: &str,
    check: SchemaPrivilegeCheck,
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

pub fn grant_acl(
    security: &mut SchemaSecurity,
    privilege: SchemaAclPrivilege,
    grantees: &[String],
    grantor: &str,
    grant_option: bool,
) {
    materialize_acl(security);
    let owner = security.role_owner.clone();
    let acl = security.acl.as_mut().expect("schema ACL was materialized");
    for grantee in grantees {
        let position = acl
            .iter()
            .position(|entry| entry.role == *grantee && acl_grantor(entry, &owner) == grantor)
            .unwrap_or_else(|| {
                acl.push(SchemaAclEntry {
                    role: grantee.clone(),
                    grantor: Some(grantor.to_string()),
                    privileges: SchemaPrivileges::default(),
                    grant_options: SchemaPrivileges::default(),
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

pub fn revoke_acl(
    security: &mut SchemaSecurity,
    privilege: SchemaAclPrivilege,
    grantees: &[String],
    grantor: &str,
    grant_option_only: bool,
    cascade: bool,
) -> Result<(), SQLError> {
    let before = grant_option_roles(security, privilege);
    materialize_acl(security);
    let owner = security.role_owner.clone();
    let acl = security.acl.as_mut().expect("schema ACL was materialized");
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
    security: &mut SchemaSecurity,
    privilege: SchemaAclPrivilege,
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
            .expect("dependent schema privileges require an explicit ACL");
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

fn remove_empty_entries(acl: &mut Vec<SchemaAclEntry>) {
    acl.retain(|entry| !entry.privileges.is_empty() || !entry.grant_options.is_empty());
}

pub fn schema_security_with_public_privileges(create: bool) -> SchemaSecurity {
    let role_owner = "uqa".to_string();
    SchemaSecurity {
        role_owner: role_owner.clone(),
        acl: Some(vec![
            SchemaAclEntry {
                role: role_owner.clone(),
                grantor: Some(role_owner.clone()),
                privileges: SchemaPrivileges::ALL,
                grant_options: SchemaPrivileges::default(),
            },
            SchemaAclEntry {
                role: "PUBLIC".into(),
                grantor: Some(role_owner),
                privileges: SchemaPrivileges {
                    usage: true,
                    create,
                },
                grant_options: SchemaPrivileges::default(),
            },
        ]),
    }
}

pub fn rewrite_schema_acl_owner(security: &mut SchemaSecurity, new_owner: &str) {
    if let Some(acl) = &mut security.acl {
        for entry in acl.iter_mut() {
            if entry.role == security.role_owner {
                entry.role = new_owner.to_string();
            }
            if entry.grantor.as_deref().unwrap_or(&security.role_owner) == security.role_owner {
                entry.grantor = Some(new_owner.to_string());
            }
        }
        let mut merged: Vec<SchemaAclEntry> = Vec::new();
        for entry in std::mem::take(acl) {
            if let Some(previous) = merged
                .iter_mut()
                .find(|previous| previous.role == entry.role && previous.grantor == entry.grantor)
            {
                previous.privileges.insert(entry.privileges);
                previous.grant_options.insert(entry.grant_options);
            } else {
                merged.push(entry);
            }
        }
        *acl = merged;
    }
    security.role_owner = new_owner.to_string();
}

pub fn apply_schema_acl(
    statement: &GrantSchemaStmt,
    grantees: &[String],
    privileges: &[SchemaAclPrivilege],
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
    current: &SchemaSecurity,
) -> Result<(SchemaSecurity, usize), SQLError> {
    let grantors = privileges
        .iter()
        .map(|privilege| {
            (
                *privilege,
                select_acl_grantor(current, *privilege, current_user, roles, memberships),
            )
        })
        .collect::<Vec<_>>();
    let grantable = grantors
        .iter()
        .filter(|(_, grantor)| grantor.is_some())
        .count();
    let mut next = current.clone();
    for (privilege, grantor) in grantors {
        let Some(grantor) = grantor else {
            continue;
        };
        if statement.is_grant {
            grant_acl(
                &mut next,
                privilege,
                grantees,
                &grantor,
                statement.grant_option,
            );
        } else {
            revoke_acl(
                &mut next,
                privilege,
                grantees,
                &grantor,
                statement.grant_option_only,
                statement.revoke_behavior == SchemaRevokeBehavior::Cascade,
            )?;
        }
    }
    Ok((next, grantable))
}

pub fn validate_schema_acl_roles(
    statement: &GrantSchemaStmt,
    grantees: &[String],
    requested_grantor: Option<&str>,
    current_user: &str,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    for role in grantees {
        if role != "PUBLIC" && !roles.contains_key(role) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{role}\" does not exist"),
            });
        }
    }
    if statement.is_grant && statement.grant_option && grantees.iter().any(|role| role == "PUBLIC")
    {
        return Err(SQLError::Routine {
            sqlstate: "0LP01".into(),
            message: "grant options can only be granted to roles".into(),
        });
    }
    if let Some(requested_grantor) = requested_grantor {
        if !roles.contains_key(requested_grantor) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{requested_grantor}\" does not exist"),
            });
        }
        if requested_grantor != current_user {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "grantor must be current user".into(),
            });
        }
    }
    Ok(())
}

pub fn schema_acl_warning(is_grant: bool, partial: bool, name: &str) -> (&'static str, String) {
    let message = match (is_grant, partial) {
        (true, true) => format!("not all privileges were granted for \"{name}\""),
        (true, false) => format!("no privileges were granted for \"{name}\""),
        (false, true) => format!("not all privileges could be revoked for \"{name}\""),
        (false, false) => format!("no privileges could be revoked for \"{name}\""),
    };
    ("WARNING", message)
}

pub fn resolve_schema_grant_targets(
    registry: &BTreeMap<String, SchemaSecurity>,
    schemas: &[String],
) -> Result<Vec<String>, SQLError> {
    let mut targets = Vec::with_capacity(schemas.len());
    for schema in schemas {
        if !registry.contains_key(schema) {
            return Err(SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            });
        }
        if !targets.contains(schema) {
            targets.push(schema.clone());
        }
    }
    Ok(targets)
}
