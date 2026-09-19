//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind legacy routine names once and validate retained owner and ACL incarnations.

use super::{bound_routine_owner, routine_grant_option_roles_for};
use crate::{
    ast::{CreateFunction, RoutineAclEntry},
    catalog::roles::{identity::RoleSubject, RoleDefinition, RoleIdentity, RoleReference},
    SQLError,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::catalog_acl::AclGrantee;

/// Only the initial catalog converter may interpret named routine authority.
#[derive(Deserialize)]
pub struct LegacyRoutineAclEntry {
    pub role: AclGrantee,
    #[serde(default)]
    pub grantor: Option<String>,
    pub grant_option: bool,
}

pub struct BoundRoutineAuthority {
    pub owner: RoleIdentity,
    pub execute_acl: Option<Vec<RoutineAclEntry>>,
}

pub fn bind_legacy_authority(
    owner: &str,
    acl: Option<Vec<LegacyRoutineAclEntry>>,
    roles: &BTreeMap<String, RoleDefinition>,
    implicit_owner_execute: bool,
) -> Result<BoundRoutineAuthority, SQLError> {
    let bind = |name: &str| {
        RoleReference::from(name)
            .bind(roles)
            .map(|role| role.identity())
    };
    let identity = bind(owner)?;
    let mut execute_acl = acl
        .map(|acl| {
            acl.into_iter()
                .map(|entry| {
                    Ok(RoutineAclEntry {
                        role: entry.role.role_name().map(bind).transpose()?,
                        grantor: bind(entry.grantor.as_deref().unwrap_or(owner))?,
                        grant_option: entry.grant_option,
                    })
                })
                .collect::<Result<Vec<_>, SQLError>>()
        })
        .transpose()?;
    if implicit_owner_execute {
        if let Some(acl) = execute_acl.as_mut() {
            if !acl.iter().any(|entry| entry.role == Some(identity)) {
                acl.push(RoutineAclEntry {
                    role: Some(identity),
                    grantor: identity,
                    grant_option: false,
                });
            }
        }
    }
    validate_acl(identity, execute_acl.as_deref())?;
    Ok(BoundRoutineAuthority {
        owner: identity,
        execute_acl,
    })
}

fn invalid(message: &str) -> SQLError {
    SQLError::Internal(format!("invalid routine authority: {message}"))
}

fn validate_acl(owner: RoleIdentity, acl: Option<&[RoutineAclEntry]>) -> Result<(), SQLError> {
    if !owner.is_valid() {
        return Err(invalid("missing owner incarnation"));
    }
    let reachable = routine_grant_option_roles_for(owner, acl);
    let mut paths = BTreeSet::new();
    for entry in acl.into_iter().flatten() {
        if !entry.grantor.is_valid() || entry.role.is_some_and(|role| !role.is_valid()) {
            return Err(invalid("missing ACL endpoint incarnation"));
        }
        if entry.role.is_none() && entry.grant_option {
            return Err(invalid("PUBLIC cannot retain a grant option"));
        }
        if !paths.insert((entry.role, entry.grantor)) {
            return Err(invalid("duplicate ACL grant path"));
        }
        if !reachable.contains(&entry.grantor) {
            return Err(invalid("ACL grantor has no owner-rooted grant option"));
        }
    }
    Ok(())
}

pub fn validate_routine_authority_identities(definition: &CreateFunction) -> Result<(), SQLError> {
    validate_acl(
        bound_routine_owner(definition)?,
        definition.execute_acl.as_deref(),
    )
}

fn authority_roles(definition: &CreateFunction) -> Result<BTreeSet<RoleIdentity>, SQLError> {
    let owner = bound_routine_owner(definition)?;
    let mut identities = BTreeSet::from([owner]);
    for entry in definition.execute_acl.iter().flatten() {
        identities.extend(entry.role);
        identities.insert(entry.grantor);
    }
    Ok(identities)
}

pub fn routine_role_dependencies(
    definition: &CreateFunction,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<BTreeSet<String>, SQLError> {
    validate_routine_authority_identities(definition)?;
    authority_roles(definition)?
        .into_iter()
        .map(|identity| {
            identity
                .role_name(roles)
                .map(str::to_owned)
                .ok_or_else(|| invalid("missing role incarnation"))
        })
        .collect()
}

pub fn validate_routine_authority(
    definition: &CreateFunction,
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    routine_role_dependencies(definition, roles).map(|_| ())
}

pub fn bind_routine_grantees(
    grantees: &[AclGrantee],
    roles: &BTreeMap<String, RoleDefinition>,
) -> Result<Vec<Option<RoleIdentity>>, SQLError> {
    grantees
        .iter()
        .map(|grantee| {
            grantee
                .role_name()
                .map(|name| {
                    RoleReference::from(name)
                        .bind(roles)
                        .map(|role| role.identity())
                })
                .transpose()
        })
        .collect()
}

pub fn added_routine_acl_roles(
    before: &CreateFunction,
    after: &CreateFunction,
    roles: &BTreeMap<String, RoleDefinition>,
    added: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    let mut old = authority_roles(before)?;
    old.remove(&bound_routine_owner(before)?);
    let mut new = authority_roles(after)?;
    new.remove(&bound_routine_owner(after)?);
    validate_routine_authority(after, roles)?;
    for identity in new.difference(&old) {
        added.insert(
            identity
                .role_name(roles)
                .ok_or_else(|| invalid("missing ACL incarnation"))?
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
