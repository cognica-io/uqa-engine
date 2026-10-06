//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Define each foreign key in written order after the table and all its unique keys exist.

use super::declaration::CreateTableAnalysisContext;
use crate::ast::{CreateTable, DeclaredForeignKey, ForeignKey};
use crate::schema::constraint_changes::ForeignKeyLocation;
use crate::schema::constraint_metadata::{
    assign_constraint_name, materialize_foreign_key_identity, CatalogIdentityAllocator,
    ConstraintMetadataError,
};
use crate::schema::foreign_keys::{
    column_foreign_key, validate_bound_foreign_key_definition_with_local_state,
};
use crate::SQLError;
use std::collections::BTreeSet;

pub fn define_foreign_keys(
    context: &CreateTableAnalysisContext<'_>,
    table: &mut CreateTable,
    inherited: usize,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(&table.name).map_err(SQLError::Internal)?;
    let local_columns = table.columns.clone();
    let mut held = held_names(table, inherited);
    let mut used = context
        .index_names
        .automatic_constraint_names(&table.name)?;
    used.extend(held.iter().cloned());
    // A partition's cloned constraints already have their names and catalog identities.
    for foreign_key in &mut table.foreign_keys[..inherited] {
        validate_bound_foreign_key_definition_with_local_state(
            &context.foreign_keys,
            &table.name,
            Some(&local_columns),
            Some(&table.key_constraints),
            foreign_key,
        )?;
    }
    let mut order = std::mem::take(&mut table.foreign_key_order);
    // Older serialized declarations have no written order; still validate every retained constraint.
    if order.is_empty() {
        order.extend(
            table
                .columns
                .iter()
                .filter(|column| column.references.is_some())
                .map(|column| DeclaredForeignKey::Column(column.name.clone())),
        );
        order.extend((0..table.foreign_keys.len() - inherited).map(DeclaredForeignKey::Table));
    }
    for declared in order {
        let (mut foreign_key, location) = declaration(table, declared, inherited)?;
        if let Some(name) = &foreign_key.name {
            if !held.insert(name.clone()) {
                return Err(crate::schema::check_inheritance::duplicate_check(
                    &relation.name,
                    name,
                ));
            }
            used.insert(name.clone());
        }
        assign_constraint_name(
            &mut foreign_key.name,
            (&relation.name, &foreign_key.local_columns.join("_"), "fkey"),
            &mut used,
        )
        .map_err(ConstraintMetadataError::into_sql_error)?;
        held.extend(foreign_key.name.iter().cloned());
        bind_reference(context, &table.name, &table.qualifier, &mut foreign_key)?;
        validate_bound_foreign_key_definition_with_local_state(
            &context.foreign_keys,
            &table.name,
            Some(&local_columns),
            Some(&table.key_constraints),
            &mut foreign_key,
        )?;
        materialize_foreign_key_identity(
            &mut foreign_key.object_id,
            &mut foreign_key.catalog_identity,
            allocate,
        )
        .map_err(ConstraintMetadataError::into_sql_error)?;
        store_definition(table, location, foreign_key)?;
    }
    Ok(())
}

fn bind_reference(
    context: &CreateTableAnalysisContext<'_>,
    table: &str,
    qualifier: &str,
    key: &mut ForeignKey,
) -> Result<(), SQLError> {
    let reference = &mut key.ref_table;
    let self_reference = reference == table
        || reference == qualifier
        || table
            .rsplit_once('.')
            .is_some_and(|(_, local)| local == reference);
    if self_reference {
        table.clone_into(reference);
    } else {
        *reference = context
            .foreign_keys
            .catalog
            .resolve_table_reference(reference)?;
    }
    Ok(())
}

fn lost_foreign_key(name: &str) -> SQLError {
    SQLError::Internal(format!("declared foreign key `{name}` disappeared"))
}

fn held_names(table: &CreateTable, inherited: usize) -> BTreeSet<String> {
    table
        .columns
        .iter()
        .flat_map(|column| {
            [
                column.not_null_name.clone().filter(|_| column.not_null),
                column.check_name.clone().filter(|_| column.check.is_some()),
            ]
        })
        .chain(table.checks.iter().map(|check| check.name.clone()))
        .chain(table.key_constraints.iter().map(|key| key.name.clone()))
        .chain(
            table.foreign_keys[..inherited]
                .iter()
                .map(|key| key.name.clone()),
        )
        .flatten()
        .collect()
}

fn declaration(
    table: &CreateTable,
    declared: DeclaredForeignKey,
    inherited: usize,
) -> Result<(ForeignKey, ForeignKeyLocation), SQLError> {
    Ok(match declared {
        DeclaredForeignKey::Column(name) => {
            let position = table
                .columns
                .iter()
                .position(|column| column.name == name)
                .ok_or_else(|| lost_foreign_key(&name))?;
            let column = &table.columns[position];
            let reference = column
                .references
                .as_ref()
                .ok_or_else(|| lost_foreign_key(&name))?;
            (
                column_foreign_key(column, reference),
                ForeignKeyLocation::Column(position),
            )
        }
        DeclaredForeignKey::Table(position) => {
            let position = position + inherited;
            let key = table
                .foreign_keys
                .get(position)
                .ok_or_else(|| lost_foreign_key(&position.to_string()))?;
            (key.clone(), ForeignKeyLocation::Table(position))
        }
    })
}

fn store_definition(
    table: &mut CreateTable,
    location: ForeignKeyLocation,
    foreign_key: ForeignKey,
) -> Result<(), SQLError> {
    match location {
        ForeignKeyLocation::Column(position) => {
            let reference = table.columns[position]
                .references
                .as_mut()
                .ok_or_else(|| lost_foreign_key(&position.to_string()))?;
            let [referenced_column] = foreign_key.ref_columns.as_slice() else {
                return Err(SQLError::Internal(
                    "column FOREIGN KEY did not resolve one referenced column".into(),
                ));
            };
            reference.name = foreign_key.name;
            reference.object_id = foreign_key.object_id;
            reference.catalog_identity = foreign_key.catalog_identity;
            reference.referenced_key = foreign_key.referenced_key;
            reference.referenced_index = foreign_key.referenced_index;
            reference.table = foreign_key.ref_table;
            reference.column = Some(referenced_column.clone());
        }
        ForeignKeyLocation::Table(position) => table.foreign_keys[position] = foreign_key,
    }
    Ok(())
}
