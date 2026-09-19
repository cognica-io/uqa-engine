//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read live ACL tuples after catalog waits without retaining obsolete registry guards.

use super::{context::TableGrantContext, targets::system_target};
use uqa_sql::{
    catalog::security::{table_grants::ResolvedTableGrantTarget, BoundTableSecurity},
    SQLError,
};

pub(super) fn replaces_attribute(
    context: &TableGrantContext<'_>,
    statement: &uqa_sql::ast::GrantTableStmt,
    target: &ResolvedTableGrantTarget,
    column: &str,
    command_roles: &mut uqa_sql::catalog::security::acl_command::AclCommandRoles,
) -> Result<bool, SQLError> {
    let (_, current) = security(context, target)?;
    let roles = context.roles.role_definitions();
    let resolved = super::prepared::resolve_roles(context, statement, command_roles, &roles)?;
    let memberships = context.roles.role_memberships();
    let requested =
        uqa_sql::catalog::security::table::requested_acl_privileges(&statement.privileges)?;
    let application = uqa_sql::catalog::security::table_grants::TableGrantApplication {
        statement,
        grantees: &resolved.grantees,
        requested: &requested,
        current_user: &resolved.current_user,
        roles: &roles,
        memberships: &memberships,
    };
    let before = current.resolve(&roles).map_err(SQLError::Internal)?;
    application.replaces_attribute(&before, column)
}

pub(super) fn security(
    context: &TableGrantContext<'_>,
    target: &ResolvedTableGrantTarget,
) -> Result<(Vec<String>, BoundTableSecurity), SQLError> {
    if let Some(system) = system_target(target) {
        return Ok((
            system.column_names(),
            context.system.system_relation_security(system),
        ));
    }
    let missing = || SQLError::Internal(format!("GRANT target `{}` disappeared", target.name));
    match target.kind {
        "table" => {
            let tables = context.tables.tables();
            let table = tables.retained(&target.relation).ok_or_else(missing)?;
            Ok((table.column_names(), table.security()))
        }
        "view" | "materialized view" => {
            let views = context.registry.views();
            let view = views.get(&target.relation).ok_or_else(missing)?;
            Ok((
                view.output_columns
                    .clone()
                    .ok_or_else(|| SQLError::Internal("GRANT view has no public columns".into()))?,
                view.security(),
            ))
        }
        "foreign table" => {
            let tables = context.registry.foreign_tables();
            let table = tables.get(&target.relation).ok_or_else(missing)?;
            let security = context
                .registry
                .foreign_security()
                .get(&target.relation)
                .cloned()
                .ok_or_else(missing)?;
            Ok((
                table
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect(),
                security,
            ))
        }
        _ => Err(SQLError::Internal(
            "non-table relation in ACL tuple locking".into(),
        )),
    }
}

pub(super) fn revision(
    context: &TableGrantContext<'_>,
    target: &ResolvedTableGrantTarget,
    column: Option<&str>,
) -> Result<Option<[u8; 16]>, SQLError> {
    if let Some(system) = system_target(target) {
        return Ok(super::super::system_relations::tuple_revision(
            context.system,
            system,
            column,
        ));
    }
    Ok(security(context, target)?.1.acl_revisions.get(column))
}
