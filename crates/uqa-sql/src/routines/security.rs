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
    catalog::roles::{RoleDefinition, RoleMembership, RoleMembershipKey},
    catalog::security::object_acl,
    SQLError,
};
use std::collections::BTreeMap;
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
    object_acl::privilege_allowed(owner, acl, grant_option, superuser, has_role)
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

pub fn select_routine_acl_grantor(
    definition: &CreateFunction,
    current_user: &(impl RoleSubject + ?Sized),
    roles: &BTreeMap<String, RoleDefinition>,
    memberships: &BTreeMap<RoleMembershipKey, RoleMembership>,
) -> Result<Option<RoleIdentity>, SQLError> {
    Ok(object_acl::select_grantor(
        bound_routine_owner(definition)?,
        definition.execute_acl.as_deref(),
        current_user,
        roles,
        memberships,
    ))
}

pub fn grant_routine_acl(
    definition: &mut CreateFunction,
    grantee: Option<RoleIdentity>,
    grantor: RoleIdentity,
    grant_option: bool,
) -> Result<(), SQLError> {
    let owner = bound_routine_owner(definition)?;
    object_acl::grant(
        owner,
        &mut definition.execute_acl,
        grantee,
        grantor,
        grant_option,
    );
    Ok(())
}

/// Store the default ACL explicitly, as every GRANT and REVOKE does.
pub fn make_routine_acl_explicit(definition: &mut CreateFunction) -> Result<(), SQLError> {
    let owner = bound_routine_owner(definition)?;
    definition.execute_acl = object_acl::explicit_acl(owner, definition.execute_acl.take());
    Ok(())
}

pub fn revoke_routine_acl(
    definition: &mut CreateFunction,
    grantee: Option<RoleIdentity>,
    grantor: RoleIdentity,
    grant_option_only: bool,
    cascade: bool,
) -> Result<bool, SQLError> {
    let owner = bound_routine_owner(definition)?;
    object_acl::revoke(
        owner,
        &mut definition.execute_acl,
        grantee,
        grantor,
        grant_option_only,
        cascade,
    )
}

pub fn rewrite_routine_acl_owner(
    definition: &mut CreateFunction,
    old_owner: RoleIdentity,
    new_owner: RoleIdentity,
) {
    object_acl::rewrite_owner(&mut definition.execute_acl, old_owner, new_owner);
}

pub fn routine_acl_warning(is_grant: bool, name: &str) -> crate::SQLNotice {
    object_acl::acl_warning(is_grant, name.rsplit('.').next().unwrap_or(name))
}
