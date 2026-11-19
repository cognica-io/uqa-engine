//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind legacy routine names once and validate retained owner and ACL incarnations.

use super::bound_routine_owner;
use crate::catalog::roles::RoleDefinitions;
use crate::catalog::security::object_acl;
use crate::{
    ast::{CreateFunction, RoutineAclEntry},
    catalog::roles::{identity::RoleSubject, RoleIdentity},
    SQLError,
};
use serde::Deserialize;
use std::collections::BTreeSet;
use uqa_core::catalog_acl::AclGrantee;

mod validation;
pub use validation::{validate_routine_authority, validate_routine_authority_from_input};
pub mod input;
mod legacy;
pub use legacy::bind_legacy_authority_from_input;

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
    roles: &RoleDefinitions,
    implicit_owner_execute: bool,
) -> Result<BoundRoutineAuthority, SQLError> {
    let result = crate::catalog::work::inline(legacy::bind_from(
        owner,
        acl.as_deref(),
        roles,
        implicit_owner_execute,
        &uqa_core::memory::ProductionControl::uncontrolled(),
        &mut (),
    ))?;
    let (value, memory) = result.into_parts();
    debug_assert!(
        memory.is_none(),
        "ordinary role binding has no admission owner"
    );
    Ok(value)
}

fn invalid(message: &str) -> SQLError {
    SQLError::Internal(format!("invalid routine authority: {message}"))
}

fn validate_acl(owner: RoleIdentity, acl: Option<&[RoutineAclEntry]>) -> Result<(), SQLError> {
    object_acl::validate_acl(owner, acl, "routine")
}

pub fn validate_routine_authority_identities(definition: &CreateFunction) -> Result<(), SQLError> {
    validate_acl(
        bound_routine_owner(definition)?,
        definition.execute_acl.as_deref(),
    )
}

fn authority_roles(definition: &CreateFunction) -> Result<BTreeSet<RoleIdentity>, SQLError> {
    Ok(object_acl::acl_roles(
        bound_routine_owner(definition)?,
        definition.execute_acl.as_deref(),
    ))
}

pub fn routine_role_dependencies(
    definition: &CreateFunction,
    roles: &RoleDefinitions,
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

pub fn bind_routine_grantees(
    grantees: &[AclGrantee],
    roles: &RoleDefinitions,
) -> Result<Vec<Option<RoleIdentity>>, SQLError> {
    object_acl::bind_grantees(grantees, roles)
}

pub fn added_routine_acl_roles(
    before: &CreateFunction,
    after: &CreateFunction,
    roles: &RoleDefinitions,
    added: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    object_acl::added_acl_roles(
        (bound_routine_owner(before)?, before.execute_acl.as_deref()),
        (bound_routine_owner(after)?, after.execute_acl.as_deref()),
        roles,
        "routine",
        added,
    )
}

#[cfg(test)]
mod tests;
