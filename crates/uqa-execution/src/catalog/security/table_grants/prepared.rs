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
use crate::catalog::security::system_relations::SystemPrivilegeUpdate;
use uqa_sql::{
    ast::{GrantTableStmt, GrantTableTarget},
    catalog::{
        roles::{
            guards::{RoleDefinitionRead, RoleMembershipRead},
            resolve_role_reference,
        },
        security::{
            table::{requested_acl_privileges, RequestedTablePrivileges},
            table_grants::{
                foreign_table_privilege_updates, validate_table_acl_roles,
                validate_table_grant_target_kinds, view_privilege_updates,
                ForeignTablePrivilegeUpdate, ResolvedTableGrantTarget, TableGrantApplication,
                ViewPrivilegeUpdate,
            },
        },
    },
    SQLError,
};

pub(super) struct PreparedTableGrant<'a> {
    pub updates: Vec<TablePrivilegeUpdate<'a>>,
    pub view_updates: Vec<ViewPrivilegeUpdate>,
    pub foreign_updates: Vec<ForeignTablePrivilegeUpdate>,
    pub system_updates: Vec<SystemPrivilegeUpdate>,
    pub memberships: RoleMembershipRead<'a>,
    pub roles: RoleDefinitionRead<'a>,
    pub dependencies: std::collections::BTreeSet<String>,
    pub notices: Vec<(&'static str, String)>,
}

pub(super) fn prepare<'a>(
    context: &'a TableGrantContext<'_>,
    statement: &GrantTableStmt,
    targets: &[ResolvedTableGrantTarget],
) -> Result<PreparedTableGrant<'a>, SQLError> {
    let grantees = statement
        .grantees
        .iter()
        .map(|role| resolve_role_reference(context.names, role))
        .collect::<Vec<_>>();
    let requested_grantor = statement
        .grantor
        .as_ref()
        .map(|role| resolve_role_reference(context.names, role));
    let current_user = context.names.current_user_name();
    let roles = context.roles.role_definitions();
    validate_table_acl_roles(
        statement,
        &grantees,
        requested_grantor.as_deref(),
        &current_user,
        &roles,
    )?;

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
    let updates =
        table_privilege_updates(table_targets, &application, &mut notices, &mut dependencies)?;
    let view_updates =
        view_privilege_updates(view_targets, &application, &mut notices, &mut dependencies)?;
    let foreign_updates = foreign_table_privilege_updates(
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
    Ok(PreparedTableGrant {
        updates,
        view_updates,
        foreign_updates,
        system_updates,
        memberships,
        roles,
        dependencies,
        notices,
    })
}
