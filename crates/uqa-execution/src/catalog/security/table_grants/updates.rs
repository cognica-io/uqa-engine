//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist every table-shaped ACL candidate before publishing any registry update.
use super::{
    super::system_relations::SystemPrivilegeUpdate,
    context::{TableGrantContext, TableGrantState},
};
use uqa_sql::{
    catalog::security::{
        table_grants::{ResolvedTableGrantTarget, TableGrantApplication},
        BoundTableSecurity,
    },
    SQLError,
};
pub(super) type TablePrivilegeUpdate<'a> =
    (String, Box<dyn TableGrantState + 'a>, BoundTableSecurity);

pub(super) fn system_privilege_updates(
    context: &TableGrantContext<'_>,
    targets: &[ResolvedTableGrantTarget],
    application: &TableGrantApplication<'_>,
    notices: &mut Vec<(&'static str, String)>,
    dependencies: &mut std::collections::BTreeSet<String>,
) -> Result<Vec<SystemPrivilegeUpdate>, SQLError> {
    let mut updates = Vec::new();
    for target in targets {
        let Some(relation) = super::targets::system_target(target) else {
            continue;
        };
        uqa_sql::catalog::security::table_grants::validate_requested_columns(
            &target.relation,
            &relation.column_names(),
            application.requested,
        )?;
        let current = context
            .system
            .system_relation_security(relation)
            .resolve(application.roles)
            .map_err(SQLError::Internal)?;
        let (next, grantable) = application.apply_to(target, &current)?;
        uqa_sql::catalog::security::dependencies::added_table_acl_roles(
            &current,
            &next,
            dependencies,
        );
        uqa_sql::catalog::security::system_relations::validate_security(
            relation,
            &next,
            application.roles,
        )
        .map_err(SQLError::Internal)?;
        application.record_warning(grantable, &target.relation, notices);
        let bound =
            BoundTableSecurity::bind(&next, application.roles).map_err(SQLError::Internal)?;
        for column in application
            .replaced_tuples(&current, &next)
            .into_iter()
            .filter(|column| target.includes_acl_tuple(column.as_deref()))
        {
            let acl = column.as_ref().map_or_else(
                || bound.acl.clone().unwrap_or_default(),
                |column| bound.column_acls.get(column).cloned().unwrap_or_default(),
            );
            updates.push(
                SystemPrivilegeUpdate::new(relation, column, acl)
                    .map_err(|error| SQLError::Internal(error.to_string()))?,
            );
        }
    }
    Ok(updates)
}
pub(super) fn table_privilege_updates<'a>(
    targets: Vec<(&ResolvedTableGrantTarget, Box<dyn TableGrantState + 'a>)>,
    application: &TableGrantApplication<'_>,
    notices: &mut Vec<(&'static str, String)>,
    dependencies: &mut std::collections::BTreeSet<String>,
) -> Result<Vec<TablePrivilegeUpdate<'a>>, SQLError> {
    let mut updates = Vec::new();
    for (target, table) in targets {
        let current = table
            .security()
            .resolve(application.roles)
            .map_err(SQLError::Internal)?;
        let (next, grantable) = application.apply_to(target, &current)?;
        uqa_sql::catalog::security::dependencies::added_table_acl_roles(
            &current,
            &next,
            dependencies,
        );
        application.record_warning(grantable, &target.relation, notices);
        if application
            .replaced_tuples(&current, &next)
            .iter()
            .any(|column| target.includes_acl_tuple(column.as_deref()))
        {
            updates.push((
                target.name.clone(),
                table,
                BoundTableSecurity::bind(&next, application.roles).map_err(SQLError::Internal)?,
            ));
        }
    }
    Ok(updates)
}
