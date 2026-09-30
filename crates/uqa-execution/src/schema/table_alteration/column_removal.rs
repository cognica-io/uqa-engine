//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ALTER TABLE DROP COLUMN`: the column deletion, the notice for a missing `IF EXISTS` column, and the hierarchies with which inheritors keep a column dropped with `ONLY`.

use super::{ddl_storage_error, TableAlterContext};
use crate::schema::columns::deletion::{ColumnDrop, LocalizedColumn};
use crate::schema::publication::hierarchy::{replace_hierarchy_components, HierarchySchemaChange};
use uqa_sql::{SQLError, SQLNotice};

pub(super) fn drop_column<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    column: &str,
    if_exists: bool,
    cascade: bool,
    recurse: bool,
) -> Result<(), SQLError> {
    let deletion = context.removal.deletion.catalog_removal_context();
    let outcome = crate::schema::columns::deletion::drop_column(
        &deletion,
        table,
        column,
        (if_exists, cascade, recurse),
        &mut |localized| declare_local_column(context, localized),
    )?;
    if let ColumnDrop::Missing(relation) = outcome {
        deletion.notices.lock().push(SQLNotice::notice(format!(
            "column \"{column}\" of relation \"{relation}\" does not exist, skipping"
        )));
    }
    Ok(())
}

/// Publish the hierarchy with which an inheritor keeps its copy of a column dropped with `ONLY`.
fn declare_local_column<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    localized: LocalizedColumn,
) -> Result<(), SQLError> {
    let name = localized.table.qualified_name();
    let constraints = context
        .hierarchy
        .catalog
        .try_declared_table_constraints(&name)
        .map_err(|error| ddl_storage_error("ALTER TABLE DROP COLUMN", error))?;
    replace_hierarchy_components(
        &context.hierarchy.publication,
        context.hierarchy.catalog,
        &name,
        HierarchySchemaChange {
            columns: localized.columns,
            checks: constraints.checks,
            foreign_keys: constraints.foreign_keys,
            key_constraints: constraints.key_constraints,
            hierarchy: localized.hierarchy,
        },
    )
    .map_err(|error| ddl_storage_error("ALTER TABLE DROP COLUMN", error))
}
