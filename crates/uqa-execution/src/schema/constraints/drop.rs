//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove constraints and their referencing keys in dependency order.
use super::{
    constraint_error, ddl_storage_error, find_constraint, publish_constraint_state,
    table_constraint_state, ConstraintAlterContext, ConstraintLocation, SQLError,
};
use uqa_sql::schema::constraint_changes::foreign_key_target::ForeignKeyTarget;
mod foreign_keys;
pub use foreign_keys::{
    capture_foreign_key_dependencies, drop_foreign_key_dependencies, ForeignKeyRemovalTarget,
};

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
    if super::inheritance::drop_inherited_constraint(context, &table, name, recurse, cascade)? {
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
    let (columns, constraints) = table_constraint_state(context, table)?;
    if let Some(target) = ForeignKeyTarget::by_name(&columns, &constraints, name)? {
        if direct {
            foreign_keys::ensure_direct_removal(context, table, name, target.object_id)?;
        }
        let targets =
            capture_foreign_key_dependencies(context, [(table.to_string(), name.to_string())])?;
        return foreign_keys::drop_targets(context, targets, direct);
    }
    if direct {
        if let Some(key) = constraints
            .key_constraints
            .iter()
            .find(|key| key.name.as_deref() == Some(name))
        {
            let catalog = context
                .publication
                .indexes
                .identities
                .catalog
                .current_catalog_snapshot();
            for row in catalog.catalog_indexes() {
                let definition = crate::catalog::index::index_definition(row)
                    .map_err(|error| ddl_storage_error("DROP CONSTRAINT index", error))?;
                if key.catalog_identity.is_some_and(|identity| {
                    definition.relationships.owning_constraint == Some(identity.object_id)
                }) && definition.relationships.parent_index.is_some()
                {
                    return Err(constraint_error("2BP01", format!("cannot drop constraint {name} on table {table} because its parent index requires it")));
                }
            }
        }
    }
    drop_constraint_one(context, table, name, if_exists, cascade)
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
    if let Some(target) = ForeignKeyTarget::by_name(&columns, &constraints, name)? {
        return foreign_keys::drop_one(context, table, target.object_id);
    }
    if let ConstraintLocation::Key(index) = location {
        drop_key_constraint_dependencies(
            context,
            table,
            &constraints.key_constraints[index],
            cascade,
        )?;
        // Dependent waits may refresh unrelated metadata, including renamed FK references.
        (columns, constraints) = table_constraint_state(context, table)?;
    }
    let location = find_constraint(&columns, &constraints, name).ok_or_else(|| {
        SQLError::Internal("locked constraint disappeared during dependent removal".into())
    })?;
    match location {
        ConstraintLocation::NotNull(index) => {
            uqa_sql::schema::constraint_changes::not_null_removal::validate_constraint_removal(
                table,
                &columns[index],
                &constraints,
            )?;
            columns[index].not_null = false;
            columns[index].not_null_explicit = false;
            columns[index].not_null_name = None;
            columns[index].not_null_identity = None;
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
            columns[index].check_catalog_oid = None;
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
            constraints.key_constraints.remove(index);
            constraints
                .hierarchy
                .partition_inherited_key_constraints
                .retain(|inherited| {
                    !uqa_sql::schema::constraint_metadata::identity::keys::provenance_matches(
                        &key, inherited,
                    )
                });
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
) -> Result<(), SQLError> {
    let catalog = context
        .publication
        .indexes
        .identities
        .catalog
        .current_catalog_snapshot();
    let rows = &catalog.snapshot().definitions.catalog_indexes;
    let owner = key
        .catalog_identity
        .ok_or_else(|| SQLError::Internal("key has no catalog identity".into()))?;
    let index = rows
        .values()
        .find(|row| {
            crate::catalog::index::index_definition(row)
                .ok()
                .is_some_and(|definition| {
                    definition.relationships.owning_constraint == Some(owner.object_id)
                })
        })
        .ok_or_else(|| SQLError::Internal("key has no owned index".into()))?;
    let indexes =
        crate::schema::indexes::registry::lifecycle::descendants(rows, &index.relation)
            .map_err(|error| ddl_storage_error("DROP CONSTRAINT index dependencies", error))?;
    let index_ids = indexes
        .iter()
        .map(|row| {
            crate::catalog::index::index_definition(row).map(|definition| {
                definition
                    .catalog
                    .expect("validated index")
                    .identity
                    .object_id
            })
        })
        .collect::<Result<std::collections::BTreeSet<_>, _>>()
        .map_err(|error| ddl_storage_error("DROP CONSTRAINT identity", error))?;
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
            if foreign_key
                .referenced_index
                .is_some_and(|id| index_ids.contains(&id))
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
    let targets = capture_foreign_key_dependencies(context, dependents)?;
    drop_foreign_key_dependencies(context, targets)
}
