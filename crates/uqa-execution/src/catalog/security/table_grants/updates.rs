//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist every table-shaped ACL candidate before publishing any registry update.
use super::{
    super::system_relations::{changed_columns, SystemPrivilegeUpdate},
    context::{TableGrantContext, TableGrantState},
};
use uqa_sql::{
    catalog::security::{
        table_grants::{
            ForeignTablePrivilegeUpdate, ResolvedTableGrantTarget, TableGrantApplication,
            ViewPrivilegeUpdate,
        },
        TableSecurity,
    },
    SQLError,
};
pub(super) type TablePrivilegeUpdate<'a> = (String, Box<dyn TableGrantState + 'a>, TableSecurity);

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
        let current = context.system.system_relation_security(relation);
        let (next, grantable) = application.apply(&current)?;
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
        if !application.requested.table.is_empty() {
            updates.push(
                SystemPrivilegeUpdate::new(relation, None, next.acl.clone().unwrap_or_default())
                    .map_err(|error| SQLError::Internal(error.to_string()))?,
            );
        }
        let mut columns = changed_columns(&current, &next)
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        // PostgreSQL replaces a nonempty attribute ACL even when its bits were already granted.
        columns.extend(
            application
                .requested
                .columns
                .iter()
                .map(|(_, column)| column)
                .filter(|column| {
                    next.column_acls
                        .get(*column)
                        .is_some_and(|acl| !acl.is_empty())
                })
                .cloned(),
        );
        for column in columns {
            let acl = next.column_acls.get(&column).cloned().unwrap_or_default();
            updates.push(
                SystemPrivilegeUpdate::new(relation, Some(column), acl)
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
        let current = table.security();
        let (next, grantable) = application.apply(&current)?;
        uqa_sql::catalog::security::dependencies::added_table_acl_roles(
            &current,
            &next,
            dependencies,
        );
        application.record_warning(grantable, &target.relation, notices);
        if next != current {
            updates.push((target.name.clone(), table, next));
        }
    }
    Ok(updates)
}
pub(super) fn persist_table_privilege_updates(
    context: &TableGrantContext<'_>,
    updates: &[TablePrivilegeUpdate<'_>],
    view_updates: &[ViewPrivilegeUpdate],
    foreign_updates: &[ForeignTablePrivilegeUpdate],
) -> Result<(), SQLError> {
    for (name, table, security) in updates {
        table
            .persist_security(name, security)
            .map_err(|error| SQLError::Internal(format!("persist table privileges: {error}")))?;
    }
    if let Some(catalog) = context.catalog {
        for (relation, view) in view_updates {
            if view.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                continue;
            }
            let row = crate::catalog::view::catalog_view_row(relation, view).map_err(|error| {
                SQLError::Internal(format!(
                    "serialize view privileges for `{}`: {error}",
                    relation.qualified_name()
                ))
            })?;
            catalog.save_view(&row).map_err(|error| {
                SQLError::Internal(format!(
                    "persist view privileges for `{}`: {error}",
                    relation.qualified_name()
                ))
            })?;
        }
    }
    for (relation, security) in foreign_updates {
        context.foreign.persist_security(relation, security)?;
    }
    Ok(())
}
