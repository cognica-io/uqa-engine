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
use uqa_sql::{
    ast::{GrantRoutineStmt, RoutineRevokeBehavior},
    catalog::roles::{
        resolve_acl_role_specification, resolve_role_specification, RoleReferenceNames,
    },
    routines::lifecycle::RoutineRegistry,
    routines::{
        declaration::RoutineTypeCatalog, lifecycle::binding::resolve_sql_routine_alter_target,
        security as analysis, SQLUserFunction,
    },
    SQLError,
};

pub trait RoutinePrivilegeNotices {
    fn routine_privilege_notice(&self, level: &str, message: &str);
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
        || prepare_privileges(context, stmt),
    )?;
    context
        .catalog
        .publication
        .persist_routine_definitions(&next)?;
    **registry = next;
    drop(registry);
    drop(memberships);
    drop(roles);
    for (level, message) in notices {
        context.notices.routine_privilege_notice(level, &message);
    }
    context.catalog.changes.catalog_registry_changed();
    Ok(())
}

struct RoutinePrivilegeCandidate<'a> {
    registry: RoutineRegistryWrite<'a>,
    next: RoutineRegistry,
    notices: Vec<(&'static str, String)>,
}

fn prepare_privileges<'a>(
    context: &'a RoutinePrivilegeContext<'_>,
    stmt: &GrantRoutineStmt,
) -> Result<RoleDependencyCandidate<'a, RoutinePrivilegeCandidate<'a>>, SQLError> {
    let roles = context.catalog.roles.role_definitions();
    let grantees = stmt
        .grantees
        .iter()
        .map(|role| resolve_acl_role_specification(context.role_names, role, &roles))
        .collect::<Result<Vec<_>, _>>()?;
    let requested_grantor = stmt
        .grantor
        .as_ref()
        .map(|role| resolve_role_specification(context.role_names, role).catalog_name(&roles))
        .transpose()?;
    let current_user = context.catalog.names.current_role();
    analysis::validate_routine_acl_roles(
        stmt,
        &grantees,
        requested_grantor.as_deref(),
        &current_user,
        &roles,
    )?;
    let grantees = analysis::binding::bind_routine_grantees(&grantees, &roles)?;
    let memberships = context.catalog.roles.role_memberships();
    let registry = context.catalog.registry.routines_write();
    let mut resolved = Vec::with_capacity(stmt.items.len());
    for item in &stmt.items {
        let (name, position) = resolve_sql_routine_alter_target(
            context.catalog.names,
            &registry,
            &item.name,
            item.arg_types.as_deref(),
            stmt.kind,
        )?;
        let function = registry[&name][position].clone();
        let grantor = analysis::select_routine_acl_grantor(
            &function.def,
            &current_user,
            &roles,
            &memberships,
        )?;
        resolved.push((name, position, grantor));
    }
    let mut next = registry.clone();
    let mut notices = Vec::new();
    let mut dependencies = BTreeSet::new();
    for (name, position, grantor) in resolved {
        let existing = next[&name][position].clone();
        let Some(grantor) = grantor else {
            notices.push(analysis::routine_acl_warning(
                stmt.is_grant,
                &existing.def.name,
            ));
            continue;
        };
        let mut def = existing.def.clone();
        let changed = if stmt.is_grant {
            for grantee in &grantees {
                analysis::grant_routine_acl(&mut def, *grantee, grantor, stmt.grant_option)?;
            }
            def.execute_acl != existing.def.execute_acl
        } else {
            let mut changed = false;
            for grantee in &grantees {
                changed |= analysis::revoke_routine_acl(
                    &mut def,
                    *grantee,
                    grantor,
                    stmt.grant_option_only,
                    stmt.revoke_behavior == RoutineRevokeBehavior::Cascade,
                )?;
            }
            if !changed {
                notices.push(analysis::routine_acl_warning(false, &existing.def.name));
            }
            changed
        };
        analysis::binding::added_routine_acl_roles(&existing.def, &def, &roles, &mut dependencies)?;
        if changed {
            next.get_mut(&name).expect("resolved routine key")[position] =
                Arc::new(SQLUserFunction {
                    def,
                    compiled: existing.compiled.clone(),
                });
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
