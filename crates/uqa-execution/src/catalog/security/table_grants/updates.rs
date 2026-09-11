//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist every table-shaped ACL candidate before publishing any registry update.
use super::context::{TableGrantContext, TableGrantState};
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
pub(super) fn table_privilege_updates<'a>(
    targets: Vec<(&ResolvedTableGrantTarget, Box<dyn TableGrantState + 'a>)>,
    application: &TableGrantApplication<'_>,
    notices: &mut Vec<(&'static str, String)>,
) -> Result<Vec<TablePrivilegeUpdate<'a>>, SQLError> {
    let mut updates = Vec::new();
    for (target, table) in targets {
        let current = table.security();
        let (next, grantable) = application.apply(&current)?;
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
