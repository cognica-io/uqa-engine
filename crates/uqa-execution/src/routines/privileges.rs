//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply owner and EXECUTE privilege changes through retained role and registry state.

use super::catalog::{RoutineMutationContext, RoutineRegistryWrite};
use crate::{
    catalog::security::roles::{
        dependencies::{prepare_role_dependencies, RoleDependencyCandidate},
        locking::RoleLockContext,
    },
    row_locks::shared_objects::SharedObjectLockSession,
};
use std::collections::BTreeSet;
use uqa_sql::catalog::security::acl_command::{AclCommandRoles, ResolvedAclRoles};
use uqa_sql::{
    ast::{GrantRoutineStmt, RoutineRevokeBehavior},
    catalog::roles::RoleReferenceNames,
    routines::lifecycle::RoutineRegistry,
    routines::{declaration::RoutineTypeCatalog, security as analysis},
    SQLError,
};

pub trait RoutinePrivilegeNotices {
    fn routine_privilege_notice(&self, notice: uqa_sql::SQLNotice);
}
pub struct RoutinePrivilegeContext<'a> {
    pub catalog: RoutineMutationContext<'a>,
    pub locks: &'a dyn SharedObjectLockSession,
    pub schemas: &'a dyn uqa_sql::catalog::security::ownership::RelationOwnerSchemas,
    pub types: &'a dyn RoutineTypeCatalog,
    pub role_names: &'a dyn RoleReferenceNames,
    pub notices: &'a dyn RoutinePrivilegeNotices,
}

mod ownership;
mod targets;
#[cfg(test)]
mod tests;
pub use ownership::alter_sql_routine_owner;

pub fn grant_sql_routine(
    context: &RoutinePrivilegeContext<'_>,
    stmt: &GrantRoutineStmt,
) -> Result<(), SQLError> {
    let targets = targets::bind(context, stmt)?;
    let mut command_roles = AclCommandRoles::default();
    let RoleDependencyCandidate {
        roles,
        memberships,
        value:
            RoutinePrivilegeCandidate {
                mut registry,
                next,
                notices,
            },
        ..
    } = prepare_role_dependencies(
        &RoleLockContext {
            roles: context.catalog.roles,
            session: context.locks,
        },
        || context.catalog.writer.prepare_writer(),
        || prepare_privileges(context, stmt, &targets, &mut command_roles),
    )?;
    if targets.is_empty() {
        return Ok(());
    }
    context
        .catalog
        .publication
        .persist_routine_definitions(&next)?;
    super::catalog::publication::record_changes(context.catalog.changes, &registry, &next);
    **registry = next;
    drop(registry);
    drop(memberships);
    drop(roles);
    for notice in notices {
        context.notices.routine_privilege_notice(notice);
    }
    context.catalog.changes.catalog_registry_changed();
    Ok(())
}

struct RoutinePrivilegeCandidate<'a> {
    registry: RoutineRegistryWrite<'a>,
    next: RoutineRegistry,
    notices: Vec<uqa_sql::SQLNotice>,
}

fn prepare_privileges<'a>(
    context: &'a RoutinePrivilegeContext<'_>,
    stmt: &GrantRoutineStmt,
    targets: &targets::RoutineGrantTargets,
    command_roles: &mut AclCommandRoles,
) -> Result<RoleDependencyCandidate<'a, RoutinePrivilegeCandidate<'a>>, SQLError> {
    let roles = context.catalog.roles.role_definitions();
    let ResolvedAclRoles {
        grantees,
        current_user,
        ..
    } = command_roles.resolve_validated(
        context.role_names,
        &roles,
        &stmt.grantees,
        stmt.grantor.as_ref(),
        |resolved| {
            analysis::validate_routine_acl_roles(
                stmt,
                &resolved.grantees,
                resolved.requested_grantor.as_deref(),
                &resolved.current_user,
                &roles,
            )
        },
    )?;
    analysis::grants::validate_privileges(stmt)?;
    let bound_grantees = analysis::binding::bind_routine_grantees(&grantees, &roles)?;
    let memberships = context.catalog.roles.role_memberships();
    let registry = context.catalog.registry.routines_write();
    let mut next = registry.clone();
    let mut notices = Vec::new();
    let mut dependencies = BTreeSet::new();
    for (name, position) in targets::current(targets, &next)? {
        let existing = next[&name][position].clone();
        let grantor = analysis::select_routine_acl_grantor(
            &existing.def,
            &current_user,
            &roles,
            &memberships,
        )?;
        analysis::grants::validate_target_options(stmt, &grantees)?;
        let mut def = existing.def.clone();
        if let Some(grantor) = grantor {
            if stmt.is_grant {
                for grantee in &bound_grantees {
                    analysis::grant_routine_acl(&mut def, *grantee, grantor, stmt.grant_option)?;
                }
            } else {
                for grantee in &bound_grantees {
                    analysis::revoke_routine_acl(
                        &mut def,
                        *grantee,
                        grantor,
                        stmt.grant_option_only,
                        stmt.revoke_behavior == RoutineRevokeBehavior::Cascade,
                    )?;
                }
            }
        } else {
            notices.push(analysis::routine_acl_warning(
                stmt.is_grant,
                &existing.def.name,
            ));
        }
        // The command stores the ACL even when it grants or revokes nothing.
        analysis::make_routine_acl_explicit(&mut def)?;
        analysis::binding::added_routine_acl_roles(&existing.def, &def, &roles, &mut dependencies)?;
        // `ExecGrant_common` stores a new catalog tuple even when its ACL is unchanged.
        next.get_mut(&name).expect("resolved routine key")[position] =
            super::catalog::revision::replacement(def, existing.body.clone())?;
    }
    Ok(RoleDependencyCandidate {
        value: RoutinePrivilegeCandidate {
            registry,
            next,
            notices,
        },
        memberships,
        roles,
        dependencies,
    })
}
