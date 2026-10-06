//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declaration and identity rules for newly added PRIMARY KEY and UNIQUE constraints.
use crate::{
    ast::{ColumnDef, TableKeyConstraint, TableKeyConstraintKind},
    SQLError,
};
/// The relation that ALTER TABLE adds a key to.
pub struct AddedKeyRelation<'a> {
    pub table: &'a str,
    pub columns: &'a [ColumnDef],
    pub keys: &'a [TableKeyConstraint],
    /// The relation's own partition key, when it is partitioned.
    pub partition: Option<&'a crate::ast::PartitionSpec>,
}

/// Validate a key that ALTER TABLE adds, before its index is named: its declaration as `transformIndexConstraint` checks it, a primary key's columns as the NOT NULL constraints that `ATPrepAddPrimaryKey` adds before the index, and then the checks of `DefineIndex`, which resolves the key's columns.
pub fn validate_added_key(
    relation: &AddedKeyRelation<'_>,
    key: &TableKeyConstraint,
) -> Result<(), SQLError> {
    if let Some(column) = key
        .columns
        .iter()
        .enumerate()
        .find_map(|(position, column)| key.columns[..position].contains(column).then_some(column))
    {
        return Err(definition::repeated_key_column(key.kind, column));
    }
    let column = |name: &str| relation.columns.iter().find(|column| column.name == name);
    let system = definition::is_system_column;
    if key.without_overlaps {
        if let Some(period) = key.columns.last() {
            if let Some(found) = column(period) {
                definition::validate_overlaps_column(period, Some(&found.ty))?;
            } else if system(period) {
                definition::validate_overlaps_column(period, None)?;
            }
        }
    }
    definition::validate_overlaps_key_length(key)?;
    if key.kind == TableKeyConstraintKind::PrimaryKey {
        for name in &key.columns {
            if column(name).is_some() {
                continue;
            }
            if system(name) {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: format!("cannot add not-null constraint on system column \"{name}\""),
                });
            }
            let local = uqa_core::RelationIdentity::from_legacy_name(relation.table)
                .map_err(SQLError::Internal)?;
            return Err(SQLError::Routine {
                sqlstate: "42703".into(),
                message: format!(
                    "column \"{name}\" of relation \"{}\" does not exist",
                    local.name
                ),
            });
        }
    }
    definition::validate_key_definition(
        &definition::KeyRelation {
            table: relation.table,
            columns: relation.columns,
            partition: relation.partition,
            has_primary_key: relation
                .keys
                .iter()
                .any(|existing| existing.kind == TableKeyConstraintKind::PrimaryKey),
        },
        key,
    )
}

/// The keys that ALTER TABLE ADD COLUMN declares on the new column, transformed as `transformIndexConstraints` transforms the statement: a second primary key fails with the relation's name as written, and a key repeating an earlier key's index is dropped.
pub fn transform_column_keys(
    relation: &str,
    keys: &[TableKeyConstraint],
) -> Result<Vec<TableKeyConstraint>, SQLError> {
    if keys
        .iter()
        .filter(|key| key.kind == TableKeyConstraintKind::PrimaryKey)
        .count()
        > 1
    {
        return Err(definition::multiple_primary_keys(relation));
    }
    Ok(definition::index_order(keys.to_vec()))
}

/// Apply the NOT NULL requirement of a primary key to the stored column candidate.
pub fn apply_primary_key_columns(
    table: &str,
    constraint: &TableKeyConstraint,
    columns: &mut [ColumnDef],
) -> Result<(), String> {
    if constraint.kind == TableKeyConstraintKind::PrimaryKey {
        for key_column in &constraint.columns {
            let column = columns
                .iter_mut()
                .find(|column| column.name == *key_column)
                .ok_or_else(|| {
                    format!("column `{key_column}` does not exist on table `{table}`")
                })?;
            column.not_null = true;
        }
    }
    Ok(())
}

pub mod definition;

#[cfg(test)]
mod tests;
