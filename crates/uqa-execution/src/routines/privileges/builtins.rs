//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Build builtin EXECUTE candidates with the same SQL ACL policy as user routines.

use super::candidates::GrantCandidateContext;
use crate::catalog::{
    projection::BuiltinRoutineIdentity, security::builtin_routines::BuiltinRoutinePrivilegeUpdate,
};
use uqa_core::catalog_role::RoleIdentity;
use uqa_sql::{
    ast::RoutineRevokeBehavior,
    catalog::{
        roles::role_inherits,
        security::{builtin_routines::BuiltinRoutineSecurities, object_acl},
    },
    routines::security::grants,
    SQLError,
};

pub(super) fn prepare(
    context: &mut GrantCandidateContext<'_>,
    target: BuiltinRoutineIdentity,
    registry: &mut BuiltinRoutineSecurities,
) -> Result<BuiltinRoutinePrivilegeUpdate, SQLError> {
    let stmt = context.statement;
    let roles = context.roles;
    let memberships = context.memberships;
    let current = context.current_user;
    let owner = RoleIdentity::BOOTSTRAP;
    let before = registry
        .get(&target.oid)
        .map(|entry| entry.execute_acl.as_slice());
    let grantor = object_acl::select_grantor(owner, before, current, roles, memberships);
    if grantor.is_none()
        && !object_acl::privilege_allowed(&owner, before, false, false, |role| {
            role_inherits(roles, memberships, current, role)
        })
    {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for function {}", target.name),
        });
    }
    grants::validate_target_options(stmt, context.grantees)?;
    let mut next = before.map(<[_]>::to_vec);
    if let Some(grantor) = grantor {
        for grantee in context.bound_grantees {
            if stmt.is_grant {
                object_acl::grant(owner, &mut next, *grantee, grantor, stmt.grant_option);
            } else {
                object_acl::revoke(
                    owner,
                    &mut next,
                    *grantee,
                    grantor,
                    stmt.grant_option_only,
                    stmt.revoke_behavior == RoutineRevokeBehavior::Cascade,
                )?;
            }
        }
    } else {
        context
            .notices
            .push(object_acl::acl_warning(stmt.is_grant, target.name));
    }
    let next = object_acl::explicit_acl(owner, next).expect("explicit routine ACL");
    object_acl::added_acl_roles(
        (owner, before),
        (owner, Some(&next)),
        roles,
        "builtin routine",
        context.dependencies,
    )?;
    let update = BuiltinRoutinePrivilegeUpdate::new(target.oid, next)
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    registry.insert(target.oid, update.entry.clone());
    Ok(update)
}
