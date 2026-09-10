//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove constraints and their referencing keys in dependency order.
use super::{
    checks, constraint_error, ddl_storage_error, find_constraint, publish_constraint_state,
    table_constraint_state, ConstraintAlterContext, ConstraintLocation, SQLError,
};
use uqa_sql::schema::constraint_changes::foreign_key_object_id;

pub fn drop_constraint(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    if_exists: bool,
    cascade: bool,
    recurse: bool,
) -> Result<(), SQLError> {
    let table = context
        .publication
        .catalog
        .resolve_table_name(table)
        .map_err(|error| ddl_storage_error("DROP CONSTRAINT relation lookup", error))?
        .unwrap_or_else(|| table.to_string());
    if let Some(trigger) = context.access.constraint_trigger_name(&table, name)? {
        let relation = uqa_core::RelationIdentity::from_legacy_name(&table).map_err(|error| {
            SQLError::Internal(format!(
                "decode constraint-trigger relation `{table}`: {error}"
            ))
        })?;
        return Err(constraint_error(
            "2BP01",
            format!(
                "cannot drop constraint {name} on table {} because trigger {} on table {} requires it\nHINT: You can drop trigger {} on table {} instead.",
                relation.name,
                trigger,
                relation.name,
                trigger,
                relation.name
            ),
        ));
    }
    if checks::drop_check(context, &table, name, recurse, cascade)? {
        return Ok(());
    }
    drop_constraint_group(context, &table, name, if_exists, cascade, true)
}

pub fn drop_constraint_dependency(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
) -> Result<(), SQLError> {
    drop_constraint_group(context, table, name, true, true, false)
}

fn drop_constraint_group(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    if_exists: bool,
    cascade: bool,
    direct: bool,
) -> Result<(), SQLError> {
    let targets = constraint_drop_targets(context, table, name, if_exists, direct)?;
    if direct {
        for target in targets.iter().filter(|target| target.as_str() != table) {
            context
                .access
                .ensure_no_pending_events(target, "ALTER TABLE")?;
        }
    }
    for target in targets {
        drop_constraint_one(context, &target, name, true, cascade)?;
    }
    Ok(())
}

fn constraint_drop_targets(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    if_exists: bool,
    direct: bool,
) -> Result<Vec<String>, SQLError> {
    let (columns, constraints) = table_constraint_state(context, table)?;
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
            let (parent_columns, parent_constraints) = table_constraint_state(context, parent)?;
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
        let relation = uqa_core::RelationIdentity::from_legacy_name(table).map_err(|error| {
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
    for candidate in context
        .relations
        .table_names()
        .map_err(|error| ddl_storage_error("DROP CONSTRAINT partition lookup", error))?
    {
        if candidate == table {
            continue;
        }
        let (candidate_columns, candidate_constraints) =
            table_constraint_state(context, &candidate)?;
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

pub fn drop_constraint_one(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    if_exists: bool,
    cascade: bool,
) -> Result<(), SQLError> {
    let (mut columns, mut constraints) = table_constraint_state(context, table)?;
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
        context
            .access
            .ensure_no_pending_events(&referenced_table, "ALTER TABLE")?;
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
            columns[index].not_null_is_local = true;
        }
        ConstraintLocation::ColumnCheck(index) => {
            columns[index].check = None;
            columns[index].check_name = None;
            columns[index].check_enforced = true;
            columns[index].check_validated = true;
            columns[index].check_no_inherit = false;
            columns[index].check_is_local = true;
            columns[index].check_object_id = None;
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
            let local_dependents = drop_key_constraint_dependencies(context, table, &key, cascade)?;
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
    publish_constraint_state(context, table, columns, constraints)
}

fn drop_key_constraint_dependencies(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    key: &uqa_sql::ast::TableKeyConstraint,
    cascade: bool,
) -> Result<std::collections::BTreeSet<String>, SQLError> {
    let canonical = context
        .publication
        .catalog
        .resolve_table_name(table)
        .map_err(|error| ddl_storage_error("DROP CONSTRAINT dependency", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut dependents = Vec::new();
    for referrer in context
        .relations
        .table_names()
        .map_err(|error| ddl_storage_error("DROP CONSTRAINT dependency", error))?
    {
        for foreign_key in context
            .catalog
            .try_foreign_keys(&referrer)
            .map_err(|error| ddl_storage_error("DROP CONSTRAINT dependency", error))?
        {
            if foreign_key.ref_table == canonical
                && foreign_key
                    .referenced_key
                    .as_ref()
                    .is_none_or(|name| key.name.as_ref() == Some(name))
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
            drop_constraint_dependency(context, &referrer, &name)?;
        }
    }
    Ok(local_dependents)
}
