//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine execution authorization, owner transitions, and grant-option reachability.

use super::{registration::RoutineSupportAuthority, routine_kind, routine_local_name};
use crate::catalog::roles::identity::RoleSubject;
use crate::catalog::roles::{RoleIdentity, RoleReference};

pub mod binding;
use crate::{
    ast::{
        AlterRoutineOwnerStmt, AlterRoutineStmt, CreateFunction, GrantRoutineStmt, RoutineAclEntry,
    },
    catalog::roles::{role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey},
    SQLError,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::catalog_acl::AclGrantee;

pub trait RoutineExecutionAuthority: RoutineSupportAuthority {
    fn current_role(&self) -> RoleReference;
    fn current_user_has_role_identity_privileges(
        &self,
        role: crate::catalog::roles::RoleIdentity,
    ) -> bool;
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
        &bound_routine_owner(definition)?,
        definition.execute_acl.as_deref(),
        false,
        authority.current_user_is_superuser(),
        |role| authority.current_user_has_role_identity_privileges(*role),
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
    owner: &RoleIdentity,
    acl: Option<&[RoutineAclEntry]>,
    grant_option: bool,
    superuser: bool,
    has_role: impl Fn(&RoleIdentity) -> bool,
) -> bool {
    if superuser || (grant_option && has_role(owner)) {
        return true;
    }
    acl.map_or(!grant_option, |acl| {
        acl.iter().any(|entry| {
            (!grant_option || entry.grant_option)
                && ((entry.role.is_none() && !grant_option)
                    || entry.role.as_ref().is_some_and(&has_role))
        })
    })
}

pub fn bound_routine_owner(definition: &CreateFunction) -> Result<RoleIdentity, SQLError> {
    definition
        .owner
        .filter(|owner| owner.is_valid())
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "routine `{}` has no bound catalog owner",
                definition.name
            ))
        })
}

pub fn validate_routine_acl_roles(
    stmt: &GrantRoutineStmt,
    grantees: &[AclGrantee],
    requested_grantor: Option<&str>,
    current_user: &(impl RoleSubject + ?Sized),
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    for role in grantees {
        if role
            .role_name()
            .is_some_and(|name| !roles.contains_key(name))
        {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role \"{role}\" does not exist"),
            });
        }
    }
    if stmt.is_grant && stmt.grant_option && grantees.iter().any(AclGrantee::is_public) {
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

fn materialize_routine_acl(
    definition: &mut CreateFunction,
) -> Result<&mut Vec<RoutineAclEntry>, SQLError> {
    let owner = bound_routine_owner(definition)?;
    Ok(definition.execute_acl.get_or_insert_with(|| {
        vec![
            RoutineAclEntry {
                role: None,
                grantor: owner,
                grant_option: false,
            },
            RoutineAclEntry {
                role: Some(owner),
                grantor: owner,
                grant_option: false,
            },
        ]
    }))
}

fn routine_grant_option_roles(
    definition: &CreateFunction,
) -> Result<BTreeSet<RoleIdentity>, SQLError> {
    Ok(routine_grant_option_roles_for(
        bound_routine_owner(definition)?,
        definition.execute_acl.as_deref(),
    ))
}

pub(super) fn routine_grant_option_roles_for(
    owner: RoleIdentity,
    acl: Option<&[RoutineAclEntry]>,
) -> BTreeSet<RoleIdentity> {
    let mut reachable = BTreeSet::from([owner]);
    let Some(acl) = acl else {
        return reachable;
    };
    loop {
        let mut changed = false;
        for entry in acl {
            if let Some(role) = entry.role {
                if entry.grant_option && reachable.contains(&entry.grantor) {
                    changed |= reachable.insert(role);
                }
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
) -> Result<Option<RoleIdentity>, SQLError> {
    let owner = bound_routine_owner(definition)?;
    if role_inherits(roles, memberships, current_user, &owner) {
        return Ok(Some(owner));
    }
    let grant_options = routine_grant_option_roles(definition)?;
    if let Some(identity) = current_user
        .role_definition(roles)
        .map(RoleDefinition::identity)
        .filter(|identity| grant_options.contains(identity))
    {
        return Ok(Some(identity));
    }
    Ok(definition.execute_acl.as_ref().and_then(|acl| {
        acl.iter()
            .filter_map(|entry| entry.role)
            .filter(|role| grant_options.contains(role))
            .find(|role| role_inherits(roles, memberships, current_user, role))
    }))
}

pub fn grant_routine_acl(
    definition: &mut CreateFunction,
    grantee: Option<RoleIdentity>,
    grantor: RoleIdentity,
    grant_option: bool,
) -> Result<(), SQLError> {
    let owner = bound_routine_owner(definition)?;
    if definition.execute_acl.is_none() && grantee.is_none() && grantor == owner && !grant_option {
        return Ok(());
    }
    let acl = materialize_routine_acl(definition)?;
    if let Some(entry) = acl
        .iter_mut()
        .find(|entry| entry.role == grantee && entry.grantor == grantor)
    {
        entry.grant_option |= grant_option;
    } else {
        acl.push(RoutineAclEntry {
            role: grantee,
            grantor,
            grant_option,
        });
    }
    Ok(())
}

pub fn revoke_routine_acl(
    definition: &mut CreateFunction,
    grantee: Option<RoleIdentity>,
    grantor: RoleIdentity,
    grant_option_only: bool,
    cascade: bool,
) -> Result<bool, SQLError> {
    let before_grant_options = routine_grant_option_roles(definition)?;
    let acl = materialize_routine_acl(definition)?;
    let Some(position) = acl
        .iter()
        .position(|entry| entry.role == grantee && entry.grantor == grantor)
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
    before_grant_options: &BTreeSet<RoleIdentity>,
    cascade: bool,
) -> Result<(), SQLError> {
    loop {
        let current_grant_options = routine_grant_option_roles(definition)?;
        let lost = before_grant_options
            .difference(&current_grant_options)
            .copied()
            .collect::<BTreeSet<_>>();
        if lost.is_empty() {
            return Ok(());
        }
        let dependent_exists = definition
            .execute_acl
            .as_ref()
            .is_some_and(|acl| acl.iter().any(|entry| lost.contains(&entry.grantor)));
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
            .retain(|entry| !lost.contains(&entry.grantor));
    }
}

pub fn rewrite_routine_acl_owner(
    definition: &mut CreateFunction,
    old_owner: RoleIdentity,
    new_owner: RoleIdentity,
) {
    let Some(acl) = definition.execute_acl.as_mut() else {
        return;
    };
    for entry in acl.iter_mut() {
        if entry.role == Some(old_owner) {
            entry.role = Some(new_owner);
        }
        if entry.grantor == old_owner {
            entry.grantor = new_owner;
        }
    }
    let mut merged: Vec<RoutineAclEntry> = Vec::with_capacity(acl.len());
    for entry in std::mem::take(acl) {
        if let Some(existing) = merged
            .iter_mut()
            .find(|existing| existing.role == entry.role && existing.grantor == entry.grantor)
        {
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

    fn role(value: u8) -> RoleIdentity {
        RoleIdentity {
            oid: i64::from(value),
            object_id: [value; 16],
        }
    }
    fn grant(grantee: u8, grantor: u8) -> RoutineAclEntry {
        RoutineAclEntry {
            role: Some(role(grantee)),
            grantor: role(grantor),
            grant_option: true,
        }
    }

    #[test]
    fn routine_grant_option_reachability_requires_an_owner_root() {
        let disconnected_cycle = [grant(2, 3), grant(3, 2)];
        assert_eq!(
            routine_grant_option_roles_for(role(1), Some(&disconnected_cycle)),
            BTreeSet::from([role(1)])
        );
        let rooted_cycle = [grant(2, 1), grant(3, 2), grant(2, 3)];
        assert_eq!(
            routine_grant_option_roles_for(role(1), Some(&rooted_cycle)),
            BTreeSet::from([role(1), role(2), role(3)])
        );
    }

    #[test]
    fn routine_grant_option_reachability_accepts_an_independent_owner_path() {
        let acl = [grant(2, 1), grant(3, 2), grant(3, 1), grant(4, 3)];
        assert_eq!(
            routine_grant_option_roles_for(role(1), Some(&acl[2..])),
            BTreeSet::from([role(1), role(3), role(4)])
        );
    }
}
