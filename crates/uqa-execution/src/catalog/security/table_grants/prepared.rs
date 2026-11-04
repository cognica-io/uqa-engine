//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate owned ACL candidates while retaining their authorization guards.

use super::{
    context::TableGrantContext,
    targets::{
        validated_foreign_table_grant_targets, validated_table_grant_targets,
        validated_view_grant_targets,
    },
    updates::{system_privilege_updates, table_privilege_updates, TablePrivilegeUpdate},
};
use crate::catalog::security::{
    roles::dependencies::RoleDependencyCandidate, system_relations::SystemPrivilegeUpdate,
};
use uqa_sql::catalog::roles::RoleDefinitions;
use uqa_sql::catalog::security::acl_command::{AclCommandRoles, ResolvedAclRoles};
use uqa_sql::{
    ast::{GrantTableStmt, GrantTableTarget},
    catalog::security::{
        table::{requested_acl_privileges, RequestedTablePrivileges},
        table_grants::{
            foreign_table_privilege_updates, validate_table_acl_roles,
            validate_table_grant_target_kinds, view_privilege_updates, ForeignTablePrivilegeUpdate,
            ResolvedTableGrantTarget, TableGrantApplication, ViewPrivilegeUpdate,
        },
    },
    SQLError,
};

pub(super) struct PreparedTableGrant<'a> {
    pub updates: Vec<TablePrivilegeUpdate<'a>>,
    pub view_updates: Vec<ViewPrivilegeUpdate>,
    pub views: Option<uqa_sql::catalog::stored_view::ViewDefinitions>,
    pub foreign_updates: Vec<ForeignTablePrivilegeUpdate>,
    pub system_updates: Vec<SystemPrivilegeUpdate>,
    pub system: Option<SystemPrivilegeCandidate<'a>>,
    pub acl_updates: Vec<super::tuples::RelationPrivilegeUpdate>,
    pub notices: Vec<uqa_sql::SQLNotice>,
}

pub(super) struct SystemPrivilegeCandidate<'a> {
    pub registry: Box<
        dyn std::ops::DerefMut<
                Target = uqa_sql::catalog::security::system_relations::SystemRelationSecurities,
            > + 'a,
    >,
    pub next: uqa_sql::catalog::security::system_relations::SystemRelationSecurities,
}

pub(super) fn resolve_roles(
    context: &TableGrantContext<'_>,
    statement: &GrantTableStmt,
    command_roles: &mut AclCommandRoles,
    roles: &RoleDefinitions,
) -> Result<ResolvedAclRoles, SQLError> {
    command_roles.resolve_validated(
        context.names,
        roles,
        &statement.grantees,
        statement.grantor.as_ref(),
        |resolved| {
            validate_table_acl_roles(
                statement,
                &resolved.grantees,
                resolved.requested_grantor.as_deref(),
                &resolved.current_user,
                roles,
            )
        },
    )
}

pub(super) fn prepare<'a>(
    context: &'a TableGrantContext<'_>,
    statement: &GrantTableStmt,
    targets: &[ResolvedTableGrantTarget],
    command_roles: &mut AclCommandRoles,
) -> Result<RoleDependencyCandidate<'a, PreparedTableGrant<'a>>, SQLError> {
    let roles = context.roles.role_definitions();
    let ResolvedAclRoles {
        grantees,
        current_user,
        ..
    } = resolve_roles(context, statement, command_roles, &roles)?;

    validate_table_grant_target_kinds(statement, targets)?;
    let has_table_relations = targets.iter().any(|target| {
        matches!(
            target.kind,
            "table" | "view" | "materialized view" | "foreign table"
        )
    });
    let requested_privileges = if has_table_relations
        || matches!(
            statement.target,
            GrantTableTarget::AllTablesInSchemas { .. }
        ) {
        requested_acl_privileges(&statement.privileges)?
    } else {
        RequestedTablePrivileges {
            table: Vec::new(),
            columns: Vec::new(),
        }
    };
    let memberships = context.roles.role_memberships();
    let table_targets = validated_table_grant_targets(context, targets, &requested_privileges)?;
    let view_targets = validated_view_grant_targets(context, targets, &requested_privileges)?;
    let foreign_targets =
        validated_foreign_table_grant_targets(context, targets, &requested_privileges)?;
    let mut notices = Vec::new();
    let mut dependencies = std::collections::BTreeSet::new();
    let application = TableGrantApplication {
        statement,
        grantees: &grantees,
        requested: &requested_privileges,
        current_user: &current_user,
        roles: &roles,
        memberships: &memberships,
    };
    let mut updates =
        table_privilege_updates(table_targets, &application, &mut notices, &mut dependencies)?;
    let mut view_updates =
        view_privilege_updates(view_targets, &application, &mut notices, &mut dependencies)?;
    let mut foreign_updates = foreign_table_privilege_updates(
        foreign_targets,
        &application,
        &mut notices,
        &mut dependencies,
    )?;
    let system_updates = system_privilege_updates(
        context,
        targets,
        &application,
        &mut notices,
        &mut dependencies,
    )?;
    let acl_updates = super::tuples::prepare(
        context,
        targets,
        &application,
        &mut updates,
        &mut view_updates,
        &mut foreign_updates,
    )?;
    let views = if view_updates.is_empty() {
        None
    } else {
        let mut next = (**context.views.views_read()).clone();
        for (relation, view) in &view_updates {
            next.insert(relation.clone(), view.clone())?;
        }
        Some(next)
    };
    let system = if system_updates.is_empty() {
        None
    } else {
        let registry = context.system.system_relation_securities_write();
        let mut next = registry.clone();
        for update in &system_updates {
            update.publish(&mut next)?;
        }
        Some(SystemPrivilegeCandidate { registry, next })
    };
    Ok(RoleDependencyCandidate {
        value: PreparedTableGrant {
            updates,
            view_updates,
            views,
            foreign_updates,
            system_updates,
            system,
            acl_updates,
            notices,
        },
        memberships,
        roles,
        dependencies,
    })
}
