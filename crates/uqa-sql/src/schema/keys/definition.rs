//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The checks of a PRIMARY KEY or UNIQUE constraint's declaration and of its index, in the order `PostgreSQL`'s `transformIndexConstraint` and `DefineIndex` apply them.

use crate::ast::{
    ColumnDef, ColumnType, GeneratedColumnKind, PartitionSpec, TableKeyConstraint,
    TableKeyConstraintKind,
};
use crate::schema::columns::POSTGRES_SYSTEM_COLUMNS;
use crate::SQLError;

/// `column "c" named in key does not exist`, for a key or included column that the relation lacks.
pub fn missing_key_column(column: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42703".into(),
        message: format!("column \"{column}\" named in key does not exist"),
    }
}

/// A key column named twice in one key.
pub fn repeated_key_column(kind: TableKeyConstraintKind, column: &str) -> SQLError {
    let constraint = match kind {
        TableKeyConstraintKind::PrimaryKey => "primary key",
        TableKeyConstraintKind::Unique => "unique",
    };
    SQLError::Routine {
        sqlstate: "42701".into(),
        message: format!("column \"{column}\" appears twice in {constraint} constraint"),
    }
}

/// A second primary key, reported with the relation's own name.
pub fn multiple_primary_keys(table: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P16".into(),
        message: format!("multiple primary keys for table \"{table}\" are not allowed"),
    }
}

/// The `WITHOUT OVERLAPS` column of a key must be a range or multirange column; a system column never is.
pub fn validate_overlaps_column(column: &str, ty: Option<&ColumnType>) -> Result<(), SQLError> {
    if matches!(ty, Some(ColumnType::Range(_) | ColumnType::Multirange(_))) {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "42804".into(),
        message: format!(
            "column \"{column}\" in WITHOUT OVERLAPS is not a range or multirange type"
        ),
    })
}

/// A `WITHOUT OVERLAPS` key compares at least one column by equality besides its period.
pub fn validate_overlaps_key_length(key: &TableKeyConstraint) -> Result<(), SQLError> {
    if !key.without_overlaps || key.columns.len() >= 2 {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "42601".into(),
        message: "constraint using WITHOUT OVERLAPS needs at least two columns".into(),
    })
}

/// What owns an index, which names the index in an attribute error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOwner {
    Key(TableKeyConstraintKind),
    Index,
}

/// `DefineIndex` builds no index on a system column or on a virtual generated column. `columns` are the relation's columns, and a name outside them is a system column, which binding has already admitted.
pub fn validate_index_attribute(
    columns: &[ColumnDef],
    name: &str,
    owner: IndexOwner,
) -> Result<(), SQLError> {
    let Some(column) = columns.iter().find(|column| column.name == name) else {
        if POSTGRES_SYSTEM_COLUMNS.contains(&name) {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "index creation on system columns is not supported".into(),
            });
        }
        return Err(SQLError::Internal(format!(
            "index attribute `{name}` is not a column"
        )));
    };
    if column
        .generated
        .as_ref()
        .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual)
    {
        let message = match owner {
            IndexOwner::Key(TableKeyConstraintKind::PrimaryKey) => {
                "primary keys on virtual generated columns are not supported"
            }
            IndexOwner::Key(TableKeyConstraintKind::Unique) => {
                "unique constraints on virtual generated columns are not supported"
            }
            IndexOwner::Index => "indexes on virtual generated columns are not supported",
        };
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: message.into(),
        });
    }
    Ok(())
}

/// The relation a key's index is built on.
pub struct KeyRelation<'a> {
    /// The relation's catalog name.
    pub table: &'a str,
    pub columns: &'a [ColumnDef],
    /// The relation's own partition key, when it is partitioned.
    pub partition: Option<&'a PartitionSpec>,
    /// The relation already has a primary key that a new one would conflict with: `index_check_primary_key` looks for one when ALTER TABLE adds the key or a partition declares it.
    pub has_primary_key: bool,
}

/// Validate a key as `DefineIndex` does before it builds the key's index on `relation`: a second primary key first, then the partition key, then each key column and each included column as an index attribute.
pub fn validate_key_definition(
    relation: &KeyRelation<'_>,
    key: &TableKeyConstraint,
) -> Result<(), SQLError> {
    if key.kind == TableKeyConstraintKind::PrimaryKey && relation.has_primary_key {
        let local = uqa_core::RelationIdentity::from_legacy_name(relation.table)
            .map_err(SQLError::Internal)?;
        return Err(multiple_primary_keys(&local.name));
    }
    if let Some(partition) = relation.partition {
        crate::schema::indexes::unique::validate_partitioned_key_constraint(
            relation.table,
            key,
            partition,
        )?;
    }
    for name in key.columns.iter().chain(&key.included_columns) {
        validate_index_attribute(relation.columns, name, IndexOwner::Key(key.kind))?;
    }
    Ok(())
}

/// A partition below the relation a key is defined on. `DefineIndex` visits each partition after its parent, siblings in partition bound order.
pub struct KeyPartition<'a> {
    pub table: &'a str,
    /// The partition's own partition key, when it is partitioned.
    pub partition: Option<&'a PartitionSpec>,
    /// The partition's keys before the new key reaches it.
    pub keys: &'a [TableKeyConstraint],
}

/// How a partition receives a key defined on its ancestor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionKeyIndex {
    /// The partition has an equivalent key, whose index becomes the partition's index of the new key.
    Adopted,
    /// The partition builds a new index for the key.
    Created,
}

/// A partition with an equivalent key adopts it. For any other partition `DefineIndex` builds a new index: a primary key must not meet the partition's own primary key, and the key must hold the partition's own partition key columns.
pub fn define_partition_key(
    partition: &KeyPartition<'_>,
    key: &TableKeyConstraint,
) -> Result<PartitionKeyIndex, SQLError> {
    if partition
        .keys
        .iter()
        .any(|existing| crate::schema::inheritance::alter::key_equivalent(existing, key))
    {
        return Ok(PartitionKeyIndex::Adopted);
    }
    if key.kind == TableKeyConstraintKind::PrimaryKey
        && partition
            .keys
            .iter()
            .any(|existing| existing.kind == TableKeyConstraintKind::PrimaryKey)
    {
        let local = uqa_core::RelationIdentity::from_legacy_name(partition.table)
            .map_err(SQLError::Internal)?;
        return Err(multiple_primary_keys(&local.name));
    }
    if let Some(spec) = partition.partition {
        crate::schema::indexes::unique::validate_partitioned_key_constraint(
            partition.table,
            key,
            spec,
        )?;
    }
    Ok(PartitionKeyIndex::Created)
}

/// The keys one statement declares, in the order `transformIndexConstraints` builds their indexes: the primary key first, and a key whose index would repeat an earlier key's index dropped, giving its name to that key when the earlier key has none.
pub fn index_order(mut keys: Vec<TableKeyConstraint>) -> Vec<TableKeyConstraint> {
    let mut ordered = Vec::with_capacity(keys.len());
    if let Some(position) = keys
        .iter()
        .position(|key| key.kind == TableKeyConstraintKind::PrimaryKey)
    {
        ordered.push(keys.remove(position));
    }
    for key in keys {
        match ordered
            .iter_mut()
            .find(|prior: &&mut TableKeyConstraint| same_index(prior, &key))
        {
            Some(prior) => {
                if prior.name.is_none() {
                    prior.name = key.name;
                }
            }
            None => ordered.push(key),
        }
    }
    ordered
}

/// `transformIndexConstraints` compares the indexes that keys need rather than their kinds, so a UNIQUE key that repeats the primary key adds no index.
fn same_index(left: &TableKeyConstraint, right: &TableKeyConstraint) -> bool {
    left.columns == right.columns
        && left.included_columns == right.included_columns
        && left.nulls_not_distinct == right.nulls_not_distinct
        && left.without_overlaps == right.without_overlaps
}
