//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role lifecycle ordering over retained authorization and publication guards.

use std::collections::BTreeSet;
use uqa_sql::{
    ast::{
        AlterRoleStmt, CreateRoleStmt, DropRoleStmt, GrantRoleStmt, RoleMembershipAction,
        RoleMembershipOptions,
    },
    catalog::roles::{
        definition::{self, require_role_creation},
        dependencies::ensure_roles_have_no_object_dependencies,
        memberships::{apply_grant_role_statement, require_role_attribute_authority},
        resolve_role_reference, role_can_set,
    },
    SQLError,
};
pub mod context;
use context::RoleExecutionContext;

pub fn set_role(context: &RoleExecutionContext<'_>, requested: &str) -> Result<(), SQLError> {
    if crate::routines::invocation::scopes::security_definer_active() {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: "cannot set parameter \"role\" within security-definer function".into(),
        });
    }
    let target = if requested.is_empty()
        || requested.eq_ignore_ascii_case("none")
        || requested.eq_ignore_ascii_case("default")
    {
        context.analysis.names.session_user_name()
    } else {
        requested.to_string()
    };
    let roles = context.analysis.roles.role_definitions();
    if !roles.contains_key(&target) {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("role \"{target}\" does not exist"),
        });
    }
    // The embedded connection starts as the bootstrap superuser. PostgreSQL lets a superuser session SET ROLE to any role even while a prior SET ROLE has reduced current_user.
    let session_user = context.analysis.names.session_user_name();
    let memberships = context.analysis.roles.role_memberships();
    if !role_can_set(&roles, &memberships, &session_user, &target) {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied to set role \"{target}\""),
        });
    }
    drop(memberships);
    drop(roles);
    context.publication.set_current_role(target);
    Ok(())
}

pub fn create_role(
    context: &RoleExecutionContext<'_>,
    statement: &CreateRoleStmt,
) -> Result<(), SQLError> {
    require_role_creation(&context.analysis)?;
    let current = context.analysis.names.current_user_name();
    {
        let roles = context.analysis.roles.role_definitions();
        require_role_attribute_authority(
            &roles,
            &current,
            statement.attributes.iter().copied(),
            "create role",
        )?;
    }
    context.publication.prepare_writer()?;
    let mut roles = context.registry.write_roles();
    let (next_roles, current_is_superuser) =
        definition::create_role_candidate(&roles, &current, statement)?;
    let mut memberships = context.registry.write_memberships();
    let mut next_memberships = memberships.clone();
    definition::apply_create_role_memberships(
        &context.analysis,
        statement,
        &current,
        current_is_superuser,
        &next_roles,
        &mut next_memberships,
    )?;
    context.publication.persist_roles(&next_roles)?;
    context.publication.persist_memberships(&next_memberships)?;
    **roles = next_roles;
    **memberships = next_memberships;
    drop(memberships);
    drop(roles);
    context.publication.catalog_changed();
    Ok(())
}

pub fn alter_role(
    context: &RoleExecutionContext<'_>,
    statement: &AlterRoleStmt,
) -> Result<(), SQLError> {
    if let Some(action) = statement.membership_action {
        return grant_roles(
            context,
            &GrantRoleStmt {
                granted_roles: vec![statement.name.clone()],
                grantee_roles: statement.members.clone(),
                is_grant: action == RoleMembershipAction::Add,
                options: RoleMembershipOptions::default(),
                grantor: None,
                cascade: false,
            },
        );
    }
    let name = resolve_role_reference(context.analysis.names, &statement.name);
    let current = context.analysis.names.current_user_name();
    context.publication.prepare_writer()?;
    let mut roles = context.registry.write_roles();
    let next =
        definition::alter_role_candidate(&context.analysis, &roles, &current, name, statement)?;
    context.publication.persist_roles(&next)?;
    **roles = next;
    drop(roles);
    context.publication.catalog_changed();
    Ok(())
}

pub fn drop_roles(
    context: &RoleExecutionContext<'_>,
    statement: &DropRoleStmt,
) -> Result<(), SQLError> {
    let current = context.analysis.names.current_user_name();
    let session = context.analysis.names.session_user_name();
    context.publication.prepare_writer()?;
    let mut roles = context.registry.write_roles();
    let snapshot = roles.clone();
    let names = definition::resolve_drop_role_names(
        &context.analysis,
        statement,
        &current,
        &session,
        &snapshot,
    )?;
    let names_set = names.iter().cloned().collect::<BTreeSet<_>>();
    let mut memberships = context.registry.write_memberships();
    definition::ensure_no_grantor_dependencies(&memberships, &names_set)?;
    ensure_roles_have_no_object_dependencies(context.dependencies, &names)?;
    let mut next_roles = snapshot;
    for name in &names {
        next_roles.remove(name);
    }
    let mut next_memberships = memberships.clone();
    next_memberships.retain(|_, membership| {
        !names_set.contains(&membership.role)
            && !names_set.contains(&membership.member)
            && !names_set.contains(&membership.grantor)
    });
    context.publication.persist_roles(&next_roles)?;
    context.publication.persist_memberships(&next_memberships)?;
    **roles = next_roles;
    **memberships = next_memberships;
    drop(memberships);
    drop(roles);
    context.publication.catalog_changed();
    Ok(())
}

pub fn grant_roles(
    context: &RoleExecutionContext<'_>,
    statement: &GrantRoleStmt,
) -> Result<(), SQLError> {
    context.publication.prepare_writer()?;
    let resolved = definition::bind_grant_role_statement(&context.analysis, statement);
    let roles = context.analysis.roles.role_definitions();
    let current = context.analysis.names.current_user_name();
    let mut memberships = context.registry.write_memberships();
    let mut next = memberships.clone();
    apply_grant_role_statement(&roles, &mut next, &current, &resolved)?;
    context.publication.persist_memberships(&next)?;
    **memberships = next;
    drop(memberships);
    drop(roles);
    context.publication.catalog_changed();
    Ok(())
}

#[cfg(test)]
mod tests;
