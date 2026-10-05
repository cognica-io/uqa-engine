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
use std::{collections::BTreeSet, sync::Arc};
use uqa_sql::catalog::security::acl_command::{AclCommandRoles, ResolvedAclRoles};
use uqa_sql::{
    ast::{GrantRoutineStmt, RoutineRevokeBehavior},
    catalog::roles::RoleReferenceNames,
    routines::lifecycle::RoutineRegistry,
    routines::{
        declaration::RoutineTypeCatalog, lifecycle::binding::resolve_sql_routine_alter_target,
        security as analysis, SQLUserFunction,
    },
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
pub use ownership::alter_sql_routine_owner;

pub fn grant_sql_routine(
    context: &RoutinePrivilegeContext<'_>,
    stmt: &GrantRoutineStmt,
) -> Result<(), SQLError> {
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
        || prepare_privileges(context, stmt, &mut command_roles),
    )?;
    context
        .catalog
        .publication
        .persist_routine_definitions(&next)?;
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
    let grantees = analysis::binding::bind_routine_grantees(&grantees, &roles)?;
    let memberships = context.catalog.roles.role_memberships();
    let snapshot = context.catalog.registry.routine_snapshot();
    let mut resolved = Vec::with_capacity(stmt.items.len());
    for (name, position) in resolve_privilege_targets(context, stmt, &snapshot)? {
        let grantor = analysis::select_routine_acl_grantor(
            &snapshot[&name][position].def,
            &current_user,
            &roles,
            &memberships,
        )?;
        resolved.push((name, position, grantor));
    }
    let registry = context.catalog.registry.routines_write();
    for (name, position, _) in &resolved {
        let current = registry
            .get(name)
            .and_then(|overloads| overloads.get(*position));
        if current.map(|function| function.def.object_id)
            != Some(snapshot[name][*position].def.object_id)
        {
            return Err(SQLError::Internal(format!(
                "resolved GRANT routine target `{name}` changed before mutation"
            )));
        }
    }
    let mut next = registry.clone();
    let mut notices = Vec::new();
    let mut dependencies = BTreeSet::new();
    for (name, position, grantor) in resolved {
        let existing = next[&name][position].clone();
        let mut def = existing.def.clone();
        if let Some(grantor) = grantor {
            if stmt.is_grant {
                for grantee in &grantees {
                    analysis::grant_routine_acl(&mut def, *grantee, grantor, stmt.grant_option)?;
                }
            } else {
                for grantee in &grantees {
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
        if def.execute_acl != existing.def.execute_acl {
            next.get_mut(&name).expect("resolved routine key")[position] =
                Arc::new(SQLUserFunction::new(def, existing.body.clone()));
        }
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

/// `objectNamesToOids`: every named routine resolves before any grantor is chosen. Lookup diagnostics read the type catalog, so resolution uses a snapshot rather than the registry the caller later holds for writing.
fn resolve_privilege_targets(
    context: &RoutinePrivilegeContext<'_>,
    stmt: &GrantRoutineStmt,
    registry: &RoutineRegistry,
) -> Result<Vec<(String, usize)>, SQLError> {
    stmt.items
        .iter()
        .map(|item| {
            let requested_types = uqa_sql::routines::declaration::resolve_routine_identity_types(
                context.types,
                item.arg_types.as_deref(),
                &[],
                "GRANT routine",
            )?;
            resolve_sql_routine_alter_target(
                context.catalog.names,
                registry,
                &item.name,
                requested_types.as_deref(),
                stmt.kind,
            )
        })
        .collect()
}
