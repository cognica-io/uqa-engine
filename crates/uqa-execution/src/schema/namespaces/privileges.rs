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
use uqa_sql::{
    ast::GrantSchemaStmt,
    catalog::{
        roles::{resolve_role_reference, RoleReferenceNames},
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
    let targets =
        resolve_schema_grant_targets(&context.registry.schemas_read(), &statement.schemas)?;
    let RoleDependencyCandidate {
        roles,
        memberships,
        value:
            SchemaPrivilegeCandidate {
                mut registry,
                updates,
                notices,
            },
        ..
    } = prepare_role_dependencies(
        &RoleLockContext {
            roles: context.roles,
            session: context.locks,
        },
        || context.writer.prepare_writer(),
        || prepare_privileges(context, statement, &targets),
    )?;
    for (name, security) in &updates {
        context.persistence.persist_security(name, security)?;
    }
    let changed = !updates.is_empty();
    for (name, security) in updates {
        registry.insert(name, security);
    }
    drop(registry);
    drop(memberships);
    drop(roles);
    for (level, message) in notices {
        context.notices.schema_privilege_notice(level, &message);
    }
    if changed {
        context.changes.catalog_registry_changed();
    }
    Ok(())
}

struct SchemaPrivilegeCandidate<'a> {
    registry: SchemaRegistryWrite<'a>,
    updates: Vec<(String, BoundSchemaSecurity)>,
    notices: Vec<(&'static str, String)>,
}

fn prepare_privileges<'a>(
    context: &'a SchemaPrivilegeContext<'_>,
    statement: &GrantSchemaStmt,
    targets: &[String],
) -> Result<RoleDependencyCandidate<'a, SchemaPrivilegeCandidate<'a>>, SQLError> {
    let roles = context.roles.role_definitions();
    let grantees = statement
        .grantees
        .iter()
        .map(|role| resolve_role_reference(context.session, role).catalog_name(&roles))
        .collect::<Result<Vec<_>, _>>()?;
    let requested_grantor = statement
        .grantor
        .as_ref()
        .map(|role| resolve_role_reference(context.session, role).catalog_name(&roles))
        .transpose()?;
    let current_user = context.session.current_role();
    validate_schema_acl_roles(
        statement,
        &grantees,
        requested_grantor.as_deref(),
        &current_user,
        &roles,
    )?;
    let privileges = requested_acl_privileges(&statement.privileges)?;
    let memberships = context.roles.role_memberships();
    let registry = context.registry.schemas_write();
    let mut updates = Vec::new();
    let mut notices = Vec::new();
    let mut dependencies = BTreeSet::new();
    for name in targets {
        let current = registry.get(name).cloned().ok_or_else(|| {
            SQLError::Internal(format!("schema `{name}` has no security metadata"))
        })?;
        let resolved = current.resolve(&roles).map_err(SQLError::Internal)?;
        let (next, grantable) = apply_schema_acl(
            statement,
            &grantees,
            &privileges,
            &current_user,
            &roles,
            &memberships,
            &resolved,
        )?;
        if grantable != privileges.len() {
            notices.push(schema_acl_warning(statement.is_grant, grantable != 0, name));
        }
        added_acl_roles(
            resolved.acl.as_deref().unwrap_or_default(),
            &resolved.role_owner,
            next.acl.as_deref().unwrap_or_default(),
            &next.role_owner,
            &mut dependencies,
        );
        let next = BoundSchemaSecurity::bind(&next, &roles).map_err(SQLError::Internal)?;
        if next != current {
            updates.push((name.clone(), next));
        }
    }
    Ok(RoleDependencyCandidate {
        value: SchemaPrivilegeCandidate {
            registry,
            updates,
            notices,
        },
        memberships,
        roles,
        dependencies,
    })
}
