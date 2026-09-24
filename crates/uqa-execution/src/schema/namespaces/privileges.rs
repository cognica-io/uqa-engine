//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema grant and revoke publication while retaining live authorization and registry guards.

use super::{
    NamespaceCatalogChanges, NamespaceCatalogRefresh, SchemaRegistryWrite,
    SchemaSecurityPersistence, SchemaStatementWriter,
};
use crate::{
    catalog::security::roles::{
        dependencies::{prepare_role_dependencies, RoleDependencyCandidate},
        locking::RoleLockContext,
        RoleCatalogGuards,
    },
    row_locks::shared_objects::SharedObjectLockSession,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Deref,
};
use uqa_sql::catalog::security::acl_command::{AclCommandRoles, ResolvedAclRoles};
use uqa_sql::{
    ast::GrantSchemaStmt,
    catalog::{
        roles::RoleReferenceNames,
        security::{
            dependencies::added_acl_roles,
            schema::{
                apply_schema_acl, requested_acl_privileges, resolve_schema_grant_targets,
                schema_acl_warning, validate_schema_acl_roles,
            },
            BoundSchemaSecurity,
        },
    },
    SQLError,
};

pub type SchemaRegistryRead<'a> =
    Box<dyn Deref<Target = BTreeMap<String, BoundSchemaSecurity>> + 'a>;
pub trait SchemaPrivilegeRegistry {
    fn schemas_read(&self) -> SchemaRegistryRead<'_>;
    fn schemas_write(&self) -> SchemaRegistryWrite<'_>;
}
pub trait SchemaPrivilegeNotices {
    fn schema_privilege_notice(&self, level: &str, message: &str);
}
pub struct SchemaPrivilegeContext<'a> {
    pub writer: &'a dyn SchemaStatementWriter,
    pub locks: &'a dyn SharedObjectLockSession,
    pub tuples: super::locking::SchemaLockContext<'a>,
    pub refresh: &'a dyn NamespaceCatalogRefresh,
    pub session: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub registry: &'a dyn SchemaPrivilegeRegistry,
    pub persistence: &'a dyn SchemaSecurityPersistence,
    pub notices: &'a dyn SchemaPrivilegeNotices,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

pub fn grant_schema_privileges(
    context: &SchemaPrivilegeContext<'_>,
    statement: &GrantSchemaStmt,
) -> Result<(), SQLError> {
    context
        .refresh
        .refresh_catalog()
        .map_err(|error| SQLError::Internal(format!("load schemas for privileges: {error}")))?;
    let mut targets = Vec::new();
    for name in &statement.schemas {
        resolve_schema_grant_targets(&context.registry.schemas_read(), std::slice::from_ref(name))?;
        context
            .tuples
            .bind_lifetime(name, crate::row_locks::RelationLockMode::AccessShare)?
            .ok_or_else(|| super::locking::missing(name))?;
        if !targets.contains(name) {
            targets.push(name.clone());
        }
    }
    let mut command_roles = AclCommandRoles::default();
    {
        let roles = context.roles.role_definitions();
        resolve_roles(context, statement, &mut command_roles, &roles)?;
    }
    let privileges = requested_acl_privileges(&statement.privileges)?;
    context.tuples.catalog_write()?;
    for name in targets {
        let RoleDependencyCandidate {
            roles,
            memberships,
            value,
            ..
        } = prepare_role_dependencies(
            &RoleLockContext {
                roles: context.roles,
                session: context.locks,
            },
            || context.writer.prepare_writer(),
            || prepare_privileges(context, statement, &name, &privileges, &mut command_roles),
        )?;
        drop(memberships);
        drop(roles);
        context.tuples.replace(&name, value.before)?;
        context
            .persistence
            .persist_security(&name, &value.security)?;
        context
            .registry
            .schemas_write()
            .insert(name, value.security);
        context.changes.catalog_registry_changed();
        if let Some((level, message)) = value.notice {
            context.notices.schema_privilege_notice(level, &message);
        }
    }
    Ok(())
}

struct SchemaPrivilegeCandidate {
    before: uqa_core::catalog_schema::SchemaTupleIdentity,
    security: BoundSchemaSecurity,
    notice: Option<(&'static str, String)>,
}

fn prepare_privileges<'a>(
    context: &'a SchemaPrivilegeContext<'_>,
    statement: &GrantSchemaStmt,
    name: &str,
    privileges: &[uqa_sql::catalog::security::schema::SchemaAclPrivilege],
    command_roles: &mut AclCommandRoles,
) -> Result<RoleDependencyCandidate<'a, SchemaPrivilegeCandidate>, SQLError> {
    let roles = context.roles.role_definitions();
    let ResolvedAclRoles {
        grantees,
        current_user,
        ..
    } = resolve_roles(context, statement, command_roles, &roles)?;

    let memberships = context.roles.role_memberships();
    let current = context
        .registry
        .schemas_read()
        .get(name)
        .cloned()
        .ok_or_else(|| super::locking::missing(name))?;
    let before = super::locking::tuple(&current)?;
    let resolved = current.resolve(&roles).map_err(SQLError::Internal)?;
    let (next, grantable) = apply_schema_acl(
        statement,
        &grantees,
        privileges,
        &current_user,
        &roles,
        &memberships,
        &resolved,
    )?;
    let notice = (grantable != privileges.len())
        .then(|| schema_acl_warning(statement.is_grant, grantable != 0, name));
    let mut dependencies = BTreeSet::new();
    added_acl_roles(
        resolved.acl.as_deref().unwrap_or_default(),
        &resolved.role_owner,
        next.acl.as_deref().unwrap_or_default(),
        &next.role_owner,
        &mut dependencies,
    );
    let mut security = BoundSchemaSecurity::bind(&next, &roles).map_err(SQLError::Internal)?;
    super::identity::replace_tuple(&current, &mut security)?;
    Ok(RoleDependencyCandidate {
        value: SchemaPrivilegeCandidate {
            before,
            security,
            notice,
        },
        memberships,
        roles,
        dependencies,
    })
}

fn resolve_roles(
    context: &SchemaPrivilegeContext<'_>,
    statement: &GrantSchemaStmt,
    command_roles: &mut AclCommandRoles,
    roles: &BTreeMap<String, uqa_sql::catalog::roles::RoleDefinition>,
) -> Result<ResolvedAclRoles, SQLError> {
    command_roles.resolve_validated(
        context.session,
        roles,
        &statement.grantees,
        statement.grantor.as_ref(),
        |resolved| {
            validate_schema_acl_roles(
                statement,
                &resolved.grantees,
                resolved.requested_grantor.as_deref(),
                &resolved.current_user,
                roles,
            )
        },
    )
}
