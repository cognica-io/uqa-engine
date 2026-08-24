//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint and dependent-column removal for `ALTER TABLE`.

use super::{
    constraint_error, ddl_storage_error, find_constraint, publish_constraint_state,
    table_constraint_state, ConstraintLocation, Engine, SQLError,
};

pub(super) fn drop_constraint(
    engine: &Engine,
    table: &str,
    name: &str,
    if_exists: bool,
    cascade: bool,
) -> Result<(), SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(engine, table)?;
    let Some(location) = find_constraint(&columns, &constraints, name) else {
        if if_exists {
            return Ok(());
        }
        return Err(constraint_error(
            "42704",
            format!("constraint \"{name}\" of relation \"{table}\" does not exist"),
        ));
    };
    match location {
        ConstraintLocation::NotNull(index) => {
            let column = columns[index].name.clone();
            if constraints.key_constraints.iter().any(|constraint| {
                constraint.kind == uqa_sql::ast::TableKeyConstraintKind::PrimaryKey
                    && constraint.columns.contains(&column)
            }) {
                return Err(constraint_error(
                    "42P16",
                    format!("column \"{column}\" is in a primary key"),
                ));
            }
            columns[index].not_null = false;
            columns[index].not_null_explicit = false;
            columns[index].not_null_name = None;
            columns[index].not_null_validated = true;
            columns[index].not_null_no_inherit = false;
        }
        ConstraintLocation::ColumnCheck(index) => {
            columns[index].check = None;
            columns[index].check_name = None;
            columns[index].check_enforced = true;
            columns[index].check_validated = true;
            columns[index].check_no_inherit = false;
        }
        ConstraintLocation::ColumnForeignKey(index) => columns[index].references = None,
        ConstraintLocation::TableCheck(index) => {
            constraints.checks.remove(index);
        }
        ConstraintLocation::TableForeignKey(index) => {
            constraints.foreign_keys.remove(index);
        }
        ConstraintLocation::Key(index) => {
            let key = constraints.key_constraints[index].clone();
            let local_dependents = drop_key_constraint_dependencies(engine, table, &key, cascade)?;
            for column in &mut columns {
                if column.references.as_ref().is_some_and(|reference| {
                    reference
                        .name
                        .as_ref()
                        .is_some_and(|name| local_dependents.contains(name))
                }) {
                    column.references = None;
                }
            }
            constraints.foreign_keys.retain(|foreign_key| {
                foreign_key
                    .name
                    .as_ref()
                    .is_none_or(|name| !local_dependents.contains(name))
            });
            constraints.key_constraints.remove(index);
            if key.columns.len() == 1 {
                if let Some(column) = columns
                    .iter_mut()
                    .find(|column| column.name == key.columns[0])
                {
                    match key.kind {
                        uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => {
                            column.primary_key = false;
                        }
                        uqa_sql::ast::TableKeyConstraintKind::Unique => {
                            column.unique = false;
                        }
                    }
                }
            }
        }
    }
    publish_constraint_state(engine, table, columns, constraints)
}

fn drop_key_constraint_dependencies(
    engine: &Engine,
    table: &str,
    key: &uqa_sql::ast::TableKeyConstraint,
    cascade: bool,
) -> Result<std::collections::BTreeSet<String>, SQLError> {
    let canonical = engine
        .try_resolve_table_name(table)
        .map_err(|error| ddl_storage_error("DROP CONSTRAINT dependency", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut dependents = Vec::new();
    for referrer in engine
        .table_names()
        .map_err(|error| ddl_storage_error("DROP CONSTRAINT dependency", error))?
    {
        for foreign_key in engine
            .try_foreign_keys(&referrer)
            .map_err(|error| ddl_storage_error("DROP CONSTRAINT dependency", error))?
        {
            if foreign_key.ref_table == canonical
                && foreign_key.ref_columns.len() == key.columns.len()
                && foreign_key
                    .ref_columns
                    .iter()
                    .all(|column| key.columns.contains(column))
            {
                let name = foreign_key.name.clone().ok_or_else(|| {
                    SQLError::Internal("dependent FOREIGN KEY has no durable name".into())
                })?;
                dependents.push((referrer.clone(), name));
            }
        }
    }
    if !cascade && !dependents.is_empty() {
        let dependent = &dependents[0];
        return Err(constraint_error(
            "2BP01",
            format!(
                "cannot drop constraint {} on table {table} because other objects depend on it: constraint {} on table {} depends on it",
                key.name.as_deref().unwrap_or("<unnamed>"),
                dependent.1,
                dependent.0
            ),
        ));
    }
    let mut local_dependents = std::collections::BTreeSet::new();
    for (referrer, name) in dependents {
        if referrer == canonical {
            local_dependents.insert(name);
        } else {
            drop_constraint(engine, &referrer, &name, false, true)?;
        }
    }
    Ok(local_dependents)
}

pub(super) fn drop_column_cascade(
    engine: &Engine,
    table: &str,
    column: &str,
    if_exists: bool,
) -> Result<(), SQLError> {
    if !ensure_drop_column_exists(engine, table, column, if_exists)? {
        return Ok(());
    }
    let dependents = foreign_keys_referencing_column(engine, table, column)?;
    for (referrer, name) in dependents {
        drop_constraint(engine, &referrer, &name, false, true)?;
    }
    engine
        .try_drop_column(table, column)
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
    Err(SQLError::UnknownColumn(format!("{table}.{column}")))
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
