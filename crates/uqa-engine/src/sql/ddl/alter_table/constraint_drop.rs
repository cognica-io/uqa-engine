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
    drop_constraint_group(engine, table, name, if_exists, cascade, true)
}

fn drop_constraint_dependency(engine: &Engine, table: &str, name: &str) -> Result<(), SQLError> {
    drop_constraint_group(engine, table, name, true, true, false)
}

fn drop_constraint_group(
    engine: &Engine,
    table: &str,
    name: &str,
    if_exists: bool,
    cascade: bool,
    direct: bool,
) -> Result<(), SQLError> {
    let targets = constraint_drop_targets(engine, table, name, if_exists, direct)?;
    if direct {
        for target in targets.iter().filter(|target| target.as_str() != table) {
            engine.ensure_no_pending_trigger_events(target, "ALTER TABLE")?;
        }
    }
    for target in targets {
        drop_constraint_one(engine, &target, name, true, cascade)?;
    }
    Ok(())
}

fn constraint_drop_targets(
    engine: &Engine,
    table: &str,
    name: &str,
    if_exists: bool,
    direct: bool,
) -> Result<Vec<String>, SQLError> {
    let (columns, constraints) = table_constraint_state(engine, table)?;
    let Some(location) = find_constraint(&columns, &constraints, name) else {
        if if_exists {
            return Ok(Vec::new());
        }
        return Err(constraint_error(
            "42704",
            format!("constraint \"{name}\" of relation \"{table}\" does not exist"),
        ));
    };
    let object_id = foreign_key_object_id(&columns, &constraints, location);
    let mut inherited = object_id.is_some_and(|object_id| {
        constraints
            .hierarchy
            .partition_inherited_foreign_keys
            .iter()
            .any(|foreign_key| foreign_key.object_id == Some(object_id))
    });
    if let Some(object_id) = object_id {
        for parent in &constraints.hierarchy.parents {
            let (parent_columns, parent_constraints) = table_constraint_state(engine, parent)?;
            let Some(parent_location) = find_constraint(&parent_columns, &parent_constraints, name)
            else {
                continue;
            };
            if foreign_key_object_id(&parent_columns, &parent_constraints, parent_location)
                == Some(object_id)
            {
                inherited = true;
                break;
            }
        }
    }
    if direct && inherited {
        let relation = crate::RelationIdentity::from_legacy_name(table).map_err(|error| {
            SQLError::Internal(format!(
                "decode inherited constraint relation '{table}': {error}"
            ))
        })?;
        return Err(constraint_error(
            "42P16",
            format!(
                "cannot drop inherited constraint \"{name}\" of relation \"{}\"",
                relation.name
            ),
        ));
    }
    let Some(object_id) = object_id else {
        return Ok(vec![table.to_string()]);
    };
    let mut targets = vec![table.to_string()];
    for candidate in engine
        .table_names()
        .map_err(|error| ddl_storage_error("DROP CONSTRAINT partition lookup", error))?
    {
        if candidate == table {
            continue;
        }
        let (candidate_columns, candidate_constraints) =
            table_constraint_state(engine, &candidate)?;
        let Some(candidate_location) =
            find_constraint(&candidate_columns, &candidate_constraints, name)
        else {
            continue;
        };
        let candidate_object_id = foreign_key_object_id(
            &candidate_columns,
            &candidate_constraints,
            candidate_location,
        );
        if candidate_object_id == Some(object_id) {
            targets.push(candidate);
        }
    }
    Ok(targets)
}

fn foreign_key_object_id(
    columns: &[uqa_sql::ast::ColumnDef],
    constraints: &uqa_sql::ast::TableConstraintSet,
    location: ConstraintLocation,
) -> Option<[u8; 16]> {
    match location {
        ConstraintLocation::ColumnForeignKey(index) => columns[index]
            .references
            .as_ref()
            .and_then(|reference| reference.object_id),
        ConstraintLocation::TableForeignKey(index) => constraints.foreign_keys[index].object_id,
        _ => None,
    }
}

fn drop_constraint_one(
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
    let referenced_table = match location {
        ConstraintLocation::ColumnForeignKey(index) => columns[index]
            .references
            .as_ref()
            .map(|reference| reference.table.clone()),
        ConstraintLocation::TableForeignKey(index) => {
            Some(constraints.foreign_keys[index].ref_table.clone())
        }
        _ => None,
    };
    if let Some(referenced_table) = referenced_table {
        engine.ensure_no_pending_trigger_events(&referenced_table, "ALTER TABLE")?;
    }
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
            drop_constraint_dependency(engine, &referrer, &name)?;
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
        drop_constraint_dependency(engine, &referrer, &name)?;
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
