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
    ast::GrantRoutineStmt,
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
    pub snapshot: crate::catalog::CatalogReadView,
    pub builtin_security:
        &'a dyn crate::catalog::security::builtin_routines::BuiltinRoutineSecurityState,
    pub storage: Option<&'a dyn uqa_storage::CatalogFacade>,
    pub locks: &'a dyn SharedObjectLockSession,
    pub schemas: &'a dyn uqa_sql::catalog::security::ownership::RelationOwnerSchemas,
    pub types: &'a dyn RoutineTypeCatalog,
    pub role_names: &'a dyn RoleReferenceNames,
    pub notices: &'a dyn RoutinePrivilegeNotices,
}

mod builtins;
mod candidates;
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
                mut builtin_security,
                builtin_updates,
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
    if targets
        .iter()
        .any(|target| matches!(target, targets::RoutineGrantTarget::User(_)))
    {
        context
            .catalog
            .publication
            .persist_routine_definitions(&next)?;
    }
    for update in &builtin_updates {
        update.persist(context.storage).map_err(|error| {
            SQLError::Internal(format!("persist builtin routine privilege: {error}"))
        })?;
    }
    for update in builtin_updates {
        builtin_security.insert(update.oid, update.entry);
    }
    super::catalog::publication::record_changes(context.catalog.changes, &registry, &next);
    **registry = next;
    drop(builtin_security);
    drop(registry);
    drop(memberships);
    drop(roles);
    for notice in notices {
        context.notices.routine_privilege_notice(notice);
    }
    let change = if targets
        .iter()
        .all(|target| matches!(target, targets::RoutineGrantTarget::Builtin(_)))
    {
        crate::statement::prepared::invalidation::CatalogRegistryChange::BuiltinRoutinePrivileges
    } else {
        crate::statement::prepared::invalidation::CatalogRegistryChange::Definitions
    };
    context
        .catalog
        .changes
        .catalog_registry_changed_kind(change);
    Ok(())
}

struct RoutinePrivilegeCandidate<'a> {
    builtin_security: Box<
        dyn std::ops::DerefMut<
                Target = uqa_sql::catalog::security::builtin_routines::BuiltinRoutineSecurities,
            > + 'a,
    >,
    builtin_updates: Vec<crate::catalog::security::builtin_routines::BuiltinRoutinePrivilegeUpdate>,
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
    let builtin_security = context.builtin_security.builtin_routine_securities_write();
    let mut builtin_next = builtin_security.clone();
    let mut builtin_updates = Vec::new();
    let mut notices = Vec::new();
    let mut dependencies = BTreeSet::new();
    let mut candidate = candidates::GrantCandidateContext {
        statement: stmt,
        current_user: &current_user,
        roles: &roles,
        memberships: &memberships,
        grantees: &grantees,
        bound_grantees: &bound_grantees,
        notices: &mut notices,
        dependencies: &mut dependencies,
    };
    for target in targets::current(targets, &next)? {
        match target {
            targets::CurrentRoutineGrantTarget::Builtin(target) => {
                builtin_updates.push(builtins::prepare(
                    &mut candidate,
                    target,
                    &mut builtin_next,
                )?);
            }
            targets::CurrentRoutineGrantTarget::User(name, position) => {
                candidate.user(&name, position, &mut next)?;
            }
        }
    }
    Ok(RoleDependencyCandidate {
        value: RoutinePrivilegeCandidate {
            builtin_security,
            builtin_updates,
            registry,
            next,
            notices,
        },
        memberships,
        roles,
        dependencies,
    })
}
