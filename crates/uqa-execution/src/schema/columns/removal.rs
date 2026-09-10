//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute column removal in routine, event, view, generated-column, and foreign-key dependency order.
use crate::schema::{columns::addition::ColumnAdditionState, constraints::ConstraintAlterContext};
use std::collections::BTreeSet;
use uqa_sql::schema::{
    columns::removal::{foreign_keys_referencing_column, ColumnRemovalCatalog},
    constraint_changes::constraint_error,
};
use uqa_sql::{
    ast::{CreateFunction, FunctionBinding},
    SQLError,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub trait ColumnRemovalRoutines {
    fn drop_dependents(&self, table: &str, column: &str, cascade: bool) -> Result<(), SQLError>;
    fn prepare_aliases(
        &self,
        columns: BTreeSet<(String, String)>,
        removed: &[FunctionBinding],
    ) -> Result<Vec<CreateFunction>, SQLError>;
    fn publish_rewrites(&self, rewritten: Vec<CreateFunction>) -> Result<(), SQLError>;
    fn refresh_merge_plans(&self) -> Result<(), SQLError>;
}
pub trait ColumnRemovalEvents {
    fn handle_dependencies(&self, table: &str, column: &str, cascade: bool)
        -> Result<(), SQLError>;
    fn drop_relation_rules(&self, relations: &[String]) -> StorageBackendResult<()>;
}
pub trait ColumnRemovalViews {
    fn dependents(&self, table: &str, column: &str) -> StorageBackendResult<Vec<String>>;
    fn cascade_closure(&self, views: Vec<String>) -> Result<Vec<String>, SQLError>;
    fn drop_views(&self, views: &[String]) -> Result<(), SQLError>;
}
pub trait ColumnRemovalState {
    fn generated_dependents(&self, table: &str, column: &str) -> StorageBackendResult<Vec<String>>;
    fn owned_sequence_dependents(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Vec<String>>;
    fn drop_column(&self, table: &str, column: &str, cascade: bool) -> StorageBackendResult<bool>;
}
pub struct ColumnRemovalContext<'a> {
    pub catalog: &'a dyn ColumnRemovalCatalog,
    pub fields: &'a dyn ColumnAdditionState,
    pub constraints: ConstraintAlterContext<'a>,
    pub routines: &'a dyn ColumnRemovalRoutines,
    pub events: &'a dyn ColumnRemovalEvents,
    pub views: &'a dyn ColumnRemovalViews,
    pub state: &'a dyn ColumnRemovalState,
}
fn ddl_storage_error(action: &str, error: StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &error)
}
pub fn drop_column(
    context: &ColumnRemovalContext<'_>,
    table: &str,
    column: &str,
    if_exists: bool,
    cascade: bool,
) -> Result<(), SQLError> {
    if !ensure_drop_column_exists(context, table, column, if_exists)? {
        return Ok(());
    }
    context.routines.drop_dependents(table, column, cascade)?;
    let rewritten = if context
        .fields
        .has_column(table, column)
        .map_err(|error| ddl_storage_error("DROP COLUMN routine aliases", error))?
    {
        context.routines.prepare_aliases(
            BTreeSet::from([(table.to_string(), column.to_string())]),
            &[],
        )?
    } else {
        Vec::new()
    };
    context.events.handle_dependencies(table, column, cascade)?;
    if cascade {
        // A routine/domain cycle may already have removed the root column.
        drop_column_cascade(context, table, column, true)?;
    } else {
        drop_column_restrict(context, table, column, false)?;
    }
    context.routines.publish_rewrites(rewritten)?;
    context.routines.refresh_merge_plans()
}

pub fn drop_column_cascade(
    context: &ColumnRemovalContext<'_>,
    table: &str,
    column: &str,
    if_exists: bool,
) -> Result<(), SQLError> {
    if !ensure_drop_column_exists(context, table, column, if_exists)? {
        return Ok(());
    }
    let views = context
        .views
        .dependents(table, column)
        .map_err(|error| ddl_storage_error("DROP COLUMN dependency", error))?;
    let closure = context.views.cascade_closure(views)?;
    context
        .events
        .drop_relation_rules(&closure)
        .map_err(|error| ddl_storage_error("DROP COLUMN dependency", error))?;
    context.views.drop_views(&closure)?;
    for generated in context
        .state
        .generated_dependents(table, column)
        .map_err(|error| ddl_storage_error("DROP COLUMN dependency", error))?
    {
        drop_column_cascade(context, table, &generated, true)?;
    }
    let dependents = foreign_keys_referencing_column(context.catalog, table, column)?;
    for (referrer, name) in dependents {
        crate::schema::constraints::drop::drop_constraint_dependency(
            &context.constraints,
            &referrer,
            &name,
        )?;
    }
    context
        .state
        .drop_column(table, column, true)
        .map_err(|error| ddl_storage_error("ALTER TABLE DROP COLUMN CASCADE", error))?;
    Ok(())
}

pub fn drop_column_restrict(
    context: &ColumnRemovalContext<'_>,
    table: &str,
    column: &str,
    if_exists: bool,
) -> Result<(), SQLError> {
    if !ensure_drop_column_exists(context, table, column, if_exists)? {
        return Ok(());
    }
    let sequence_dependents = context
        .state
        .owned_sequence_dependents(table, column)
        .map_err(|error| {
            ddl_storage_error("ALTER TABLE DROP COLUMN dependency preflight", error)
        })?;
    if !sequence_dependents.is_empty() {
        return Err(constraint_error(
            "2BP01",
            format!(
                "cannot drop column {column} of table {table} because other objects depend on its owned sequence: {}",
                sequence_dependents.join(", ")
            ),
        ));
    }
    if let Some((referrer, constraint)) =
        foreign_keys_referencing_column(context.catalog, table, column)?
            .into_iter()
            .next()
    {
        return Err(constraint_error(
            "2BP01",
            format!(
                "cannot drop column {column} of table {table} because other objects depend on it: constraint {constraint} on table {referrer} depends on column {column} of table {table}"
            ),
        ));
    }
    context
        .state
        .drop_column(table, column, false)
        .map_err(|error| ddl_storage_error("ALTER TABLE DROP COLUMN", error))?;
    Ok(())
}

fn ensure_drop_column_exists(
    context: &ColumnRemovalContext<'_>,
    table: &str,
    column: &str,
    if_exists: bool,
) -> Result<bool, SQLError> {
    if context
        .fields
        .has_column(table, column)
        .map_err(|error| ddl_storage_error("ALTER TABLE DROP COLUMN", error))?
    {
        return Ok(true);
    }
    if if_exists {
        return Ok(false);
    }
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    Err(constraint_error(
        "42703",
        format!(
            "column \"{column}\" of relation \"{}\" does not exist",
            relation.name
        ),
    ))
}
