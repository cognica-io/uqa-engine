//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine execution authorization, owner transitions, and grant-option reachability.

use super::{registration::RoutineSupportAuthority, routine_kind, routine_local_name};
use crate::catalog::roles::identity::RoleSubject;
use crate::catalog::roles::RoleReference;
use crate::{
    ast::{
        AlterRoutineOwnerStmt, AlterRoutineStmt, CreateFunction, GrantRoutineStmt, RoutineAclEntry,
    },
    catalog::roles::{role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey},
    SQLError,
};
use std::collections::{BTreeMap, BTreeSet};

pub trait RoutineExecutionAuthority: RoutineSupportAuthority {
    fn current_role(&self) -> RoleReference;
    fn current_user_has_role_privileges(&self, role: &str) -> bool;
}

pub fn routine_owner_identity(stmt: &AlterRoutineOwnerStmt) -> AlterRoutineStmt {
    AlterRoutineStmt {
        kind: stmt.kind,
        name: stmt.name.clone(),
        arg_types: stmt.arg_types.clone(),
        arg_type_references: stmt.arg_type_references.clone(),
        volatility: None,
        strict: None,
        security_definer: None,
        leakproof: None,
        parallel: None,
        support: None,
        config_actions: Vec::new(),
    }
}

pub fn ensure_routine_execute_privilege(
    authority: &dyn RoutineExecutionAuthority,
    definition: &CreateFunction,
) -> Result<(), SQLError> {
    ensure_routine_execute_privilege_named(
        authority,
        definition,
        &routine_local_name(&definition.name)?,
    )
}

pub fn ensure_routine_execute_privilege_named(
    authority: &dyn RoutineExecutionAuthority,
    definition: &CreateFunction,
    display_name: &str,
) -> Result<(), SQLError> {
    let allowed = routine_privilege_allowed(
        &definition.owner,
        definition.execute_acl.as_deref(),
        false,
        authority.current_user_is_superuser(),
        |role| authority.current_user_has_role_privileges(role),
    );
    if allowed {
        Ok(())
    } else {
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "permission denied for {} {}",
                routine_kind(definition),
                display_name
            ),
        })
    }
}

/// Ownership retains grant options even when the owner's explicit EXECUTE was revoked.
pub fn routine_privilege_allowed(
    owner: &str,
    acl: Option<&[RoutineAclEntry]>,
    grant_option: bool,
    superuser: bool,
    has_role: impl Fn(&str) -> bool,
) -> bool {
    if superuser || (grant_option && has_role(owner)) {
        return true;
    }
    acl.map_or(!grant_option, |acl| {
        acl.iter().any(|entry| {
            (!grant_option || entry.grant_option)
                && ((entry.role == "PUBLIC" && !grant_option) || has_role(&entry.role))
        })
    })
}

/// Legacy stored ACLs represented owner EXECUTE implicitly, including empty explicit ACLs.
pub fn migrate_implicit_routine_owner_acl(definition: &mut CreateFunction) {
    if let Some(acl) = definition.execute_acl.as_mut() {
        if !acl.iter().any(|entry| entry.role == definition.owner) {
            acl.push(RoutineAclEntry {
                role: definition.owner.clone(),
                grantor: Some(definition.owner.clone()),
                grant_option: false,
            });
        }
    }
}

pub fn validate_routine_acl_roles(
    stmt: &GrantRoutineStmt,
    grantees: &[String],
    requested_grantor: Option<&str>,
    current_user: &(impl RoleSubject + ?Sized),
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
    if stmt.is_grant && stmt.grant_option && grantees.iter().any(|role| role == "PUBLIC") {
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
        if current_user.role_name(roles) != Some(requested_grantor) {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "grantor must be current user".into(),
            });
        }
    }
    Ok(())
}

fn routine_acl_grantor<'a>(entry: &'a RoutineAclEntry, owner: &'a str) -> &'a str {
    entry.grantor.as_deref().unwrap_or(owner)
}

fn materialize_routine_acl(definition: &mut CreateFunction) -> &mut Vec<RoutineAclEntry> {
    if definition.execute_acl.is_none() {
        definition.execute_acl = Some(vec![
            RoutineAclEntry {
                role: "PUBLIC".into(),
                grantor: Some(definition.owner.clone()),
                grant_option: false,
            },
            RoutineAclEntry {
                role: definition.owner.clone(),
                grantor: Some(definition.owner.clone()),
                grant_option: false,
            },
        ]);
    }
    definition
        .execute_acl
        .as_mut()
        .expect("routine ACL was materialized")
}

fn routine_grant_option_roles(definition: &CreateFunction) -> BTreeSet<String> {
    routine_grant_option_roles_for(&definition.owner, definition.execute_acl.as_deref())
}

fn routine_grant_option_roles_for(
    owner: &str,
    acl: Option<&[RoutineAclEntry]>,
) -> BTreeSet<String> {
    let mut reachable = BTreeSet::from([owner.to_string()]);
    let Some(acl) = acl else {
        return reachable;
    };
    loop {
        let mut changed = false;
        for entry in acl {
            if entry.role != "PUBLIC"
                && entry.grant_option
                && reachable.contains(routine_acl_grantor(entry, owner))
            {
                changed |= reachable.insert(entry.role.clone());
            }
        }
        if !changed {
            return reachable;
        }
    }
}

pub fn select_routine_acl_grantor(
    definition: &CreateFunction,
    current_user: &(impl RoleSubject + ?Sized),
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> Option<String> {
    if role_inherits(roles, memberships, current_user, &definition.owner) {
        return Some(definition.owner.clone());
    }
    let grant_options = routine_grant_option_roles(definition);
    if let Some(name) = current_user
        .role_name(roles)
        .filter(|name| grant_options.contains(*name))
    {
        return Some(name.to_owned());
    }
    definition.execute_acl.as_ref().and_then(|acl| {
        acl.iter()
            .filter(|entry| entry.role != "PUBLIC" && grant_options.contains(&entry.role))
            .find(|entry| role_inherits(roles, memberships, current_user, &entry.role))
            .map(|entry| entry.role.clone())
    })
}

pub fn grant_routine_acl(
    definition: &mut CreateFunction,
    grantee: &str,
    grantor: &str,
    grant_option: bool,
) {
    if definition.execute_acl.is_none()
        && grantee == "PUBLIC"
        && grantor == definition.owner
        && !grant_option
    {
        return;
    }
    let owner = definition.owner.clone();
    let acl = materialize_routine_acl(definition);
    if let Some(entry) = acl
        .iter_mut()
        .find(|entry| entry.role == grantee && routine_acl_grantor(entry, &owner) == grantor)
    {
        entry.grant_option |= grant_option;
    } else {
        acl.push(RoutineAclEntry {
            role: grantee.to_string(),
            grantor: Some(grantor.to_string()),
            grant_option,
        });
    }
}

pub fn revoke_routine_acl(
    definition: &mut CreateFunction,
    grantee: &str,
    grantor: &str,
    grant_option_only: bool,
    cascade: bool,
) -> Result<bool, SQLError> {
    let owner = definition.owner.clone();
    let before_grant_options = routine_grant_option_roles(definition);
    let acl = materialize_routine_acl(definition);
    let Some(position) = acl
        .iter()
        .position(|entry| entry.role == grantee && routine_acl_grantor(entry, &owner) == grantor)
    else {
        return Ok(false);
    };
    if grant_option_only {
        if !acl[position].grant_option {
            return Ok(false);
        }
        acl[position].grant_option = false;
    } else {
        acl.remove(position);
    }
    revoke_dependent_routine_acl(definition, &before_grant_options, cascade)?;
    Ok(true)
}

fn revoke_dependent_routine_acl(
    definition: &mut CreateFunction,
    before_grant_options: &BTreeSet<String>,
    cascade: bool,
) -> Result<(), SQLError> {
    loop {
        let current_grant_options = routine_grant_option_roles(definition);
        let lost = before_grant_options
            .difference(&current_grant_options)
            .cloned()
            .collect::<BTreeSet<_>>();
        if lost.is_empty() {
            return Ok(());
        }
        let owner = definition.owner.clone();
        let dependent_exists = definition.execute_acl.as_ref().is_some_and(|acl| {
            acl.iter()
                .any(|entry| lost.contains(routine_acl_grantor(entry, &owner)))
        });
        if !dependent_exists {
            return Ok(());
        }
        if !cascade {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: "dependent privileges exist".into(),
            });
        }
        definition
            .execute_acl
            .as_mut()
            .expect("dependent ACLs require an explicit ACL")
            .retain(|entry| !lost.contains(routine_acl_grantor(entry, &owner)));
    }
}

pub fn rewrite_routine_acl_owner(
    definition: &mut CreateFunction,
    old_owner: &str,
    new_owner: &str,
) {
    let Some(acl) = definition.execute_acl.as_mut() else {
        return;
    };
    for entry in acl.iter_mut() {
        if entry.role == old_owner {
            entry.role = new_owner.to_string();
        }
        if entry.grantor.as_deref() == Some(old_owner) {
            entry.grantor = Some(new_owner.to_string());
        }
    }
    let mut merged: Vec<RoutineAclEntry> = Vec::with_capacity(acl.len());
    for entry in std::mem::take(acl) {
        if let Some(existing) = merged.iter_mut().find(|existing| {
            existing.role == entry.role
                && routine_acl_grantor(existing, new_owner)
                    == routine_acl_grantor(&entry, new_owner)
        }) {
            existing.grant_option |= entry.grant_option;
        } else {
            merged.push(entry);
        }
    }
    *acl = merged;
}

pub fn routine_acl_warning(is_grant: bool, name: &str) -> (&'static str, String) {
    let local_name = name.rsplit('.').next().unwrap_or(name);
    (
        "WARNING",
        if is_grant {
            format!("no privileges were granted for \"{local_name}\"")
        } else {
            format!("no privileges could be revoked for \"{local_name}\"")
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(grantee: &str, grantor: &str) -> RoutineAclEntry {
        RoutineAclEntry {
            role: grantee.into(),
            grantor: Some(grantor.into()),
            grant_option: true,
        }
    }

    #[test]
    fn routine_grant_option_reachability_requires_an_owner_root() {
        let disconnected_cycle = [grant("delegate", "leaf"), grant("leaf", "delegate")];
        assert_eq!(
            routine_grant_option_roles_for("owner", Some(&disconnected_cycle)),
            BTreeSet::from(["owner".into()])
        );

        let rooted_cycle = [
            grant("delegate", "owner"),
            grant("leaf", "delegate"),
            grant("delegate", "leaf"),
        ];
        assert_eq!(
            routine_grant_option_roles_for("owner", Some(&rooted_cycle)),
            BTreeSet::from(["delegate".into(), "leaf".into(), "owner".into()])
        );
    }

    #[test]
    fn routine_grant_option_reachability_accepts_an_independent_owner_path() {
        let acl = [
            grant("delegate", "owner"),
            grant("leaf", "delegate"),
            grant("leaf", "owner"),
            grant("tail", "leaf"),
        ];
        assert_eq!(
            routine_grant_option_roles_for("owner", Some(&acl[2..])),
            BTreeSet::from(["leaf".into(), "owner".into(), "tail".into()])
        );
    }
}
