//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply owner and EXECUTE privilege changes through retained role and registry state.

use super::catalog::RoutineMutationContext;
use std::sync::Arc;
use uqa_sql::{
    ast::{AlterRoutineOwnerStmt, GrantRoutineStmt, RoutineRevokeBehavior},
    catalog::roles::{
        require_role_exists, require_set_role, resolve_role_reference, role_inherits,
        RoleReferenceNames,
    },
    routines::{
        declaration::{resolve_alter_routine_identity_types, RoutineTypeCatalog},
        lifecycle::{binding::resolve_sql_routine_alter_target, ensure_routine_owner_as},
        security as analysis, SQLUserFunction,
    },
    SQLError,
};

pub trait RoutinePrivilegeNotices {
    fn routine_privilege_notice(&self, level: &str, message: &str);
}
pub struct RoutinePrivilegeContext<'a> {
    pub catalog: RoutineMutationContext<'a>,
    pub types: &'a dyn RoutineTypeCatalog,
    pub role_names: &'a dyn RoleReferenceNames,
    pub notices: &'a dyn RoutinePrivilegeNotices,
}

pub fn alter_sql_routine_owner(
    context: &RoutinePrivilegeContext<'_>,
    stmt: &AlterRoutineOwnerStmt,
) -> Result<(), SQLError> {
    let identity = analysis::routine_owner_identity(stmt);
    let requested_types = resolve_alter_routine_identity_types(context.types, &identity)?;
    let new_owner = resolve_role_reference(context.role_names, &stmt.new_owner);
    let current_user = context.catalog.names.current_user_name();
    context.catalog.writer.prepare_writer()?;
    let roles = context.catalog.roles.role_definitions();
    require_role_exists(&roles, &new_owner)?;
    let memberships = context.catalog.roles.role_memberships();
    let mut registry = context.catalog.registry.routines_write();
    let (name, position) = resolve_sql_routine_alter_target(
        context.catalog.names,
        &registry,
        &stmt.name,
        requested_types.as_deref(),
        stmt.kind,
    )?;
    let existing = registry[&name][position].clone();
    ensure_routine_owner_as(
        &existing.def,
        role_inherits(&roles, &memberships, &current_user, &existing.def.owner),
    )?;
    require_set_role(&roles, &memberships, &current_user, &new_owner)?;
    let mut def = existing.def.clone();
    analysis::rewrite_routine_acl_owner(&mut def, &existing.def.owner, &new_owner);
    def.owner = new_owner;
    let mut next = registry.clone();
    next.get_mut(&name).expect("resolved routine key")[position] = Arc::new(SQLUserFunction {
        def,
        compiled: existing.compiled.clone(),
    });
    context
        .catalog
        .publication
        .persist_routine_definitions(&next)?;
    **registry = next;
    drop(registry);
    drop(memberships);
    drop(roles);
    context.catalog.changes.catalog_registry_changed();
    Ok(())
}

pub fn grant_sql_routine(
    context: &RoutinePrivilegeContext<'_>,
    stmt: &GrantRoutineStmt,
) -> Result<(), SQLError> {
    let grantees = stmt
        .grantees
        .iter()
        .map(|role| resolve_role_reference(context.role_names, role))
        .collect::<Vec<_>>();
    let requested_grantor = stmt
        .grantor
        .as_ref()
        .map(|role| resolve_role_reference(context.role_names, role));
    let current_user = context.catalog.names.current_user_name();
    context.catalog.writer.prepare_writer()?;
    let roles = context.catalog.roles.role_definitions();
    analysis::validate_routine_acl_roles(
        stmt,
        &grantees,
        requested_grantor.as_deref(),
        &current_user,
        &roles,
    )?;
    let memberships = context.catalog.roles.role_memberships();
    let mut registry = context.catalog.registry.routines_write();
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
        );
        resolved.push((name, position, grantor));
    }
    let mut next = registry.clone();
    let mut notices = Vec::new();
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
                analysis::grant_routine_acl(&mut def, grantee, &grantor, stmt.grant_option);
            }
            def.execute_acl != existing.def.execute_acl
        } else {
            let mut changed = false;
            for grantee in &grantees {
                changed |= analysis::revoke_routine_acl(
                    &mut def,
                    grantee,
                    &grantor,
                    stmt.grant_option_only,
                    stmt.revoke_behavior == RoutineRevokeBehavior::Cascade,
                )?;
            }
            if !changed {
                notices.push(analysis::routine_acl_warning(false, &existing.def.name));
            }
            changed
        };
        if changed {
            next.get_mut(&name).expect("resolved routine key")[position] =
                Arc::new(SQLUserFunction {
                    def,
                    compiled: existing.compiled.clone(),
                });
        }
    }
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
