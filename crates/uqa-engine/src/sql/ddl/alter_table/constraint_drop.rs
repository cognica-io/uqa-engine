//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove columns and their routine, event, view, and key dependencies.
use super::{constraint_error, ddl_storage_error, Engine, SQLError};
use std::collections::BTreeSet;
pub(super) fn drop_constraint(
    engine: &Engine,
    table: &str,
    name: &str,
    if_exists: bool,
    cascade: bool,
    recurse: bool,
) -> Result<(), SQLError> {
    uqa_execution::schema::constraints::drop::drop_constraint(
        &engine.constraint_alter_context(),
        table,
        name,
        if_exists,
        cascade,
        recurse,
    )
}

pub(crate) fn drop_constraint_dependency(
    engine: &Engine,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
    uqa_execution::schema::constraints::drop::drop_constraint_dependency(
        &engine.constraint_alter_context(),
        table,
        name,
    )
}

pub(super) fn drop_column(
    engine: &Engine,
    table: &str,
    column: &str,
    if_exists: bool,
    cascade: bool,
) -> Result<(), SQLError> {
    if !ensure_drop_column_exists(engine, table, column, if_exists)? {
        return Ok(());
    }
    engine.drop_column_routine_dependents(table, column, cascade)?;
    let rewritten = if engine
        .try_table_has_column(table, column)
        .map_err(|error| ddl_storage_error("DROP COLUMN routine aliases", error))?
    {
        engine.prepare_routine_column_alias_drop(
            BTreeSet::from([(table.to_string(), column.to_string())]),
            &[],
        )?
    } else {
        Vec::new()
    };
    engine.handle_drop_column_event_dependencies(table, column, cascade)?;
    if cascade {
        // A routine/domain cycle may already have removed the root column.
        drop_column_cascade(engine, table, column, true)?;
    } else {
        drop_column_restrict(engine, table, column, false)?;
    }
    engine.publish_stored_routine_body_rewrites(rewritten)?;
    engine.refresh_stored_merge_target_plans()
}

pub(crate) fn drop_column_cascade(
    engine: &Engine,
    table: &str,
    column: &str,
    if_exists: bool,
) -> Result<(), SQLError> {
    if !ensure_drop_column_exists(engine, table, column, if_exists)? {
        return Ok(());
    }
    let views = engine
        .views_depending_on_column(table, column)
        .map_err(|error| ddl_storage_error("DROP COLUMN dependency", error))?;
    let closure = engine.cascade_view_closure(views)?;
    engine
        .drop_rules_depending_on_relations_inner(&closure)
        .map_err(|error| ddl_storage_error("DROP COLUMN dependency", error))?;
    engine.drop_views_inner(&closure, false)?;
    for generated in engine
        .generated_columns_referencing_column(table, column)
        .map_err(|error| ddl_storage_error("DROP COLUMN dependency", error))?
    {
        drop_column_cascade(engine, table, &generated, true)?;
    }
    let dependents = foreign_keys_referencing_column(engine, table, column)?;
    for (referrer, name) in dependents {
        drop_constraint_dependency(engine, &referrer, &name)?;
    }
    engine
        .try_drop_column_cascade(table, column)
        .map_err(|error| ddl_storage_error("ALTER TABLE DROP COLUMN CASCADE", error))?;
    Ok(())
}

pub(super) fn drop_column_restrict(
    engine: &Engine,
    table: &str,
    column: &str,
    if_exists: bool,
) -> Result<(), SQLError> {
    if !ensure_drop_column_exists(engine, table, column, if_exists)? {
        return Ok(());
    }
    let sequence_dependents = engine
        .owned_sequence_dependents_for_column(table, column)
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
    if let Some((referrer, constraint)) = foreign_keys_referencing_column(engine, table, column)?
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
    engine
        .try_drop_column(table, column)
        .map_err(|error| ddl_storage_error("ALTER TABLE DROP COLUMN", error))?;
    Ok(())
}

fn ensure_drop_column_exists(
    engine: &Engine,
    table: &str,
    column: &str,
    if_exists: bool,
) -> Result<bool, SQLError> {
    if engine
        .try_table_has_column(table, column)
        .map_err(|error| ddl_storage_error("ALTER TABLE DROP COLUMN", error))?
    {
        return Ok(true);
    }
    if if_exists {
        return Ok(false);
    }
    let relation = crate::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    Err(constraint_error(
        "42703",
        format!(
            "column \"{column}\" of relation \"{}\" does not exist",
            relation.name
        ),
    ))
}

fn foreign_keys_referencing_column(
    engine: &Engine,
    table: &str,
    column: &str,
) -> Result<Vec<(String, String)>, SQLError> {
    let canonical = engine
        .try_resolve_table_name(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE DROP COLUMN", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut dependents = Vec::new();
    for referrer in engine
        .table_names()
        .map_err(|error| ddl_storage_error("ALTER TABLE DROP COLUMN", error))?
    {
        for foreign_key in engine
            .try_foreign_keys(&referrer)
            .map_err(|error| ddl_storage_error("ALTER TABLE DROP COLUMN", error))?
        {
            if foreign_key.ref_table == canonical
                && foreign_key.ref_columns.iter().any(|name| name == column)
            {
                dependents.push((
                    referrer.clone(),
                    foreign_key.name.clone().ok_or_else(|| {
                        SQLError::Internal("dependent FOREIGN KEY has no durable name".into())
                    })?,
                ));
            }
        }
    }
    Ok(dependents)
}
