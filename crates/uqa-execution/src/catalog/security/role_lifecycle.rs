//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role lifecycle ordering over retained authorization and publication guards.

use std::collections::BTreeSet;
use uqa_sql::catalog::roles::RoleReference;
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
mod identity;
mod locking;
use context::RoleExecutionContext;

pub fn set_role(
    context: &RoleExecutionContext<'_>,
    requested: Option<&str>,
) -> Result<(), SQLError> {
    require_authorization_setting("role")?;
    let Some(target) = requested.filter(|name| *name != "none") else {
        context.publication.set_current_role(None);
        return Ok(());
    };
    let roles = context.analysis.roles.role_definitions();
    let bound = selected_role(&roles, target)?;
    let session_user = context.analysis.names.session_role();
    let memberships = context.analysis.roles.role_memberships();
    if !role_can_set(&roles, &memberships, &session_user, target) {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied to set role \"{target}\""),
        });
    }
    drop(memberships);
    drop(roles);
    context.publication.set_current_role(Some(bound));
    Ok(())
}

pub fn set_session_authorization(
    context: &RoleExecutionContext<'_>,
    requested: Option<&str>,
) -> Result<(), SQLError> {
    require_authorization_setting("session_authorization")?;
    let authenticated = context.analysis.names.authenticated_role();
    let roles = context.analysis.roles.role_definitions();
    let original = match &authenticated {
        RoleReference::Bound(identity) => identity.as_ref().clone(),
        RoleReference::Named(_) => authenticated.bind(&roles)?,
    };
    let selected =
        requested.map_or_else(|| Ok(original.clone()), |name| selected_role(&roles, name))?;
    if (selected.oid != original.oid || selected.object_id != original.object_id)
        && !uqa_sql::catalog::roles::memberships::role_is_superuser(&roles, &authenticated)
    {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: "permission denied to set session authorization".into(),
        });
    }
    drop(roles);
    context.publication.set_session_authorization(selected);
    Ok(())
}

fn require_authorization_setting(parameter: &str) -> Result<(), SQLError> {
    if crate::routines::invocation::scopes::security_definer_active() {
        return Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "cannot set parameter \"{parameter}\" within security-definer function"
            ),
        });
    }
    Ok(())
}

fn selected_role(
    roles: &std::collections::BTreeMap<String, uqa_sql::catalog::roles::RoleDefinition>,
    name: &str,
) -> Result<uqa_sql::catalog::roles::identity::RoleBinding, SQLError> {
    let role = roles.get(name).ok_or_else(|| SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("role \"{name}\" does not exist"),
    })?;
    uqa_sql::catalog::roles::identity::RoleBinding::from_definition(role)
}

pub fn create_role(
    context: &RoleExecutionContext<'_>,
    statement: &CreateRoleStmt,
) -> Result<(), SQLError> {
    require_role_creation(&context.analysis)?;
    let current = context.analysis.names.current_role();
    {
        let roles = context.analysis.roles.role_definitions();
        require_role_attribute_authority(
            &roles,
            &current,
            statement.attributes.iter().copied(),
            "create role",
        )?;
    }
    let definition = identity::reserve_definition(context, statement)?;
    require_role_creation(&context.analysis)?;
    context.publication.prepare_writer()?;
    let mut roles = context.registry.write_roles();
    require_role_attribute_authority(
        &roles,
        &current,
        statement.attributes.iter().copied(),
        "create role",
    )?;
    let (next_roles, current_is_superuser) =
        definition::create_role_candidate(&roles, &current, definition)?;
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
    context.publication.persist_roles(&roles, &next_roles)?;
    if **memberships != next_memberships {
        context.publication.persist_memberships(&next_memberships)?;
    }
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
    let current = context.analysis.names.current_role();
    context.publication.prepare_writer()?;
    let mut roles = context.registry.write_roles();
    let next = definition::alter_role_candidate(
        &context.analysis,
        &roles,
        &current,
        name.catalog_name(&roles)?,
        statement,
    )?;
    context.publication.persist_roles(&roles, &next)?;
    **roles = next;
    drop(roles);
    context.publication.catalog_changed();
    Ok(())
}

pub fn drop_roles(
    context: &RoleExecutionContext<'_>,
    statement: &DropRoleStmt,
) -> Result<(), SQLError> {
    let current = context.analysis.names.current_role();
    let session = context.analysis.names.session_role();
    let names = locking::lock_drop_targets(context, statement, &current, &session)?;
    context.publication.prepare_writer()?;
    let mut roles = context.registry.write_roles();
    let snapshot = roles.clone();
    for name in &names {
        definition::require_role_drop_authority(
            &context.analysis,
            &snapshot,
            &current,
            &session,
            name,
        )?;
    }
    let names_set = names.iter().cloned().collect::<BTreeSet<_>>();
    let mut memberships = context.registry.write_memberships();
    definition::ensure_no_grantor_dependencies(&memberships, &names_set)?;
    ensure_roles_have_no_object_dependencies(context.dependencies, &names)?;
    for name in &names {
        let role = super::roles::locking::RoleBinding::from_definition(&snapshot[name])?;
        if context
            .temporary_roles
            .peer_temporary_role_reference(role.oid)?
        {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{name}\" cannot be dropped because some objects depend on it"
                ),
            });
        }
    }
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
    context.publication.persist_roles(&roles, &next_roles)?;
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
    let roles = context.analysis.roles.role_definitions();
    let resolved =
        definition::bind_grant_role_statement(context.analysis.names, &roles, statement)?;
    let current = context.analysis.names.current_role();
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
