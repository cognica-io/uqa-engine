//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role lifecycle ordering over retained authorization and publication guards.

use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{
    ast::{AlterRoleStmt, CreateRoleStmt, DropRoleStmt, GrantRoleStmt},
    catalog::roles::{
        definition::{self, require_role_creation},
        memberships::require_role_attribute_authority,
        resolve_role_specification, role_can_set,
    },
    SQLError,
};
pub mod context;
mod identity;
mod locking;
mod memberships;
mod tuples;
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
    let roles = context.analysis.roles.role_definitions();
    require_role_attribute_authority(
        &roles,
        &current,
        statement.attributes.iter().copied(),
        "create role",
    )?;
    let (_, current_is_superuser) =
        definition::create_role_candidate(&roles, &current, definition.clone())?;
    drop(roles);
    memberships::create(
        context,
        current,
        statement,
        definition,
        current_is_superuser,
    )
}

pub fn alter_role(
    context: &RoleExecutionContext<'_>,
    statement: &AlterRoleStmt,
) -> Result<(), SQLError> {
    if statement.membership_action.is_some() {
        return memberships::alter_group(context, statement);
    }
    let name = resolve_role_specification(context.analysis.names, &statement.name);
    let current = context.analysis.names.current_role();
    let (original, replacement) = {
        let roles = context.analysis.roles.role_definitions();
        let name = name.catalog_name(&roles)?;
        let mut next = definition::alter_role_candidate(
            &context.analysis,
            &roles,
            &current,
            name.clone(),
            statement,
        )?;
        (
            uqa_sql::catalog::roles::tuple::RoleTuple::bind(&roles[&name])?,
            next.remove(&name).expect("validated role alteration"),
        )
    };
    tuples::lock(context, &original)?;
    context.publication.prepare_writer()?;
    let mut roles = context.registry.write_roles();
    original.revalidate(&roles)?;
    let mut next = roles.clone();
    next.insert(replacement.name.clone(), replacement);
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
    let targets = tuples::prepare_drop(context, &names, &current, &session)?;
    for target in &targets {
        tuples::lock(context, target)?;
    }
    let mut roles = context.registry.write_roles();
    let identities = targets
        .iter()
        .map(|target| {
            target
                .revalidate(&roles)
                .map(uqa_sql::catalog::roles::RoleDefinition::identity)
        })
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    let mut memberships = context.registry.write_memberships();
    let mut next_roles = roles.clone();
    next_roles.retain(|_, role| !identities.contains(&role.identity()));
    let mut next_memberships = memberships.clone();
    next_memberships.retain(|_, membership| {
        !identities.contains(&membership.role.identity())
            && !identities.contains(&membership.member.identity())
            && !identities.contains(&membership.grantor.identity())
    });
    context.publication.persist_roles(&roles, &next_roles)?;
    context
        .publication
        .persist_memberships(&memberships, &next_memberships)?;
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
    memberships::grant(
        context,
        statement,
        statement
            .granted_roles
            .iter()
            .cloned()
            .map(RoleReference::Named)
            .collect(),
    )
}

#[cfg(test)]
mod tests;
