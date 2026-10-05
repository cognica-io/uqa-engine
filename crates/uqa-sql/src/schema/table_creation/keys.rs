//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The PRIMARY KEY and UNIQUE constraints of a new table. `transformIndexConstraints` validates the declared keys before the relation exists, and `DefineIndex` checks and names each key's index after `DefineRelation` has cloned the keys of a partition's parent.

use crate::ast::{ColumnDef, ColumnType, CreateTable, TableKeyConstraint, TableKeyConstraintKind};
use crate::schema::columns::POSTGRES_SYSTEM_COLUMNS;
use crate::schema::constraint_metadata::{
    identity::materialize_key_identity, materialize_foreign_key_identity, CatalogIdentityAllocator,
    ConstraintMetadataError,
};
use crate::schema::indexes::names::{ConstraintIndexNamer, IndexNameCatalog};
use crate::schema::indexes::unique::{
    validate_partitioned_key_constraint, validate_partitioned_unique_key, PartitionedUniqueKey,
};
use crate::schema::inheritance::InheritanceContext;
use crate::schema::keys::definition::{
    index_order, missing_key_column, multiple_primary_keys, repeated_key_column,
    validate_key_definition, validate_overlaps_column, validate_overlaps_key_length, KeyRelation,
};
use crate::SQLError;

#[cfg(test)]
mod tests;

/// What a new table takes from its parents before its own definitions: the columns whose expressions the parents give, and the keys and foreign keys a partition clones from its parent.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InheritedDefinitions {
    /// The columns whose default or generation expression a parent gives and the statement does not replace, which `heap_create_with_catalog` stores with the relation.
    pub expressions: Vec<String>,
    /// The parent's keys, which precede the declared keys of the table.
    pub keys: usize,
    /// The parent's foreign keys, which precede the declared foreign keys of the table.
    pub foreign_keys: usize,
    /// The key attributes of the parent's unique indexes that no key constraint owns.
    pub unique_indexes: Vec<Vec<crate::ast::IndexKey>>,
}

/// Validate the keys a CREATE TABLE declares, in declaration order, then order them as `transformIndexConstraints` does: the primary key first, and a key whose index would repeat an earlier key's index dropped, giving its name to that key when the earlier key has none.
pub fn transform_declared_keys(
    context: &InheritanceContext<'_>,
    table: &mut CreateTable,
) -> Result<(), SQLError> {
    let mut columns = KeyColumns {
        context,
        declared: &table.columns,
        parents: &table.hierarchy.parents,
        inherited: Vec::new(),
    };
    let mut primary_key = false;
    for key in &table.key_constraints {
        if key.kind == TableKeyConstraintKind::PrimaryKey {
            if primary_key {
                return Err(multiple_primary_keys(&table.qualifier));
            }
            primary_key = true;
        }
        validate_declared_key(&mut columns, key)?;
    }
    table.key_constraints = index_order(std::mem::take(&mut table.key_constraints));
    // The column flags follow the keys that survive; `define_declared_keys` sets them again.
    for column in &mut table.columns {
        column.primary_key = false;
        column.unique = false;
    }
    Ok(())
}

/// The columns a declared key may name: the statement's columns, the system columns, and then each parent's columns, a parent being read only when a key names a column that no earlier source has.
struct KeyColumns<'s, 'c> {
    context: &'s InheritanceContext<'c>,
    declared: &'s [ColumnDef],
    parents: &'s [String],
    inherited: Vec<Vec<ColumnDef>>,
}

enum KeyColumn {
    Column(ColumnType),
    System,
}

impl KeyColumns<'_, '_> {
    fn find(&mut self, name: &str) -> Result<Option<KeyColumn>, SQLError> {
        if let Some(column) = self.declared.iter().find(|column| column.name == name) {
            return Ok(Some(KeyColumn::Column(column.ty.clone())));
        }
        if POSTGRES_SYSTEM_COLUMNS.contains(&name) {
            return Ok(Some(KeyColumn::System));
        }
        for (position, requested) in self.parents.iter().enumerate() {
            if position == self.inherited.len() {
                let parent = self.context.catalog.resolve_parent(requested)?;
                let columns = self
                    .context
                    .partitions
                    .catalog
                    .try_describe_table(&parent)
                    .map_err(|error| {
                        SQLError::Internal(format!("read inherited row type: {error}"))
                    })?
                    .ok_or_else(|| SQLError::UnknownTable(parent.clone()))?;
                self.inherited.push(columns);
            }
            if let Some(column) = self.inherited[position]
                .iter()
                .find(|column| column.name == name)
            {
                return Ok(Some(KeyColumn::Column(column.ty.clone())));
            }
        }
        Ok(None)
    }
}

fn validate_declared_key(
    columns: &mut KeyColumns<'_, '_>,
    key: &TableKeyConstraint,
) -> Result<(), SQLError> {
    for (position, name) in key.columns.iter().enumerate() {
        let Some(column) = columns.find(name)? else {
            return Err(missing_key_column(name));
        };
        if key.columns[..position].contains(name) {
            return Err(repeated_key_column(key.kind, name));
        }
        if key.without_overlaps && position + 1 == key.columns.len() {
            let ty = match &column {
                KeyColumn::Column(ty) => Some(ty),
                KeyColumn::System => None,
            };
            validate_overlaps_column(name, ty)?;
        }
    }
    validate_overlaps_key_length(key)?;
    for name in &key.included_columns {
        if columns.find(name)?.is_none() {
            return Err(missing_key_column(name));
        }
    }
    Ok(())
}

/// A declared primary key makes each of its columns NOT NULL through a constraint of the new table: an inherited NOT NULL becomes local and takes the name generated for the new table, as `AddRelationNotNullConstraints` merges the declaration with the inherited constraint.
pub fn declare_primary_key_not_null(columns: &mut [ColumnDef], keys: &[TableKeyConstraint]) {
    for key in keys
        .iter()
        .filter(|key| key.kind == TableKeyConstraintKind::PrimaryKey)
    {
        for name in &key.columns {
            let Some(column) = columns.iter_mut().find(|column| column.name == *name) else {
                continue;
            };
            if column.not_null && column.not_null_is_local {
                continue;
            }
            if column.not_null {
                column.not_null_name = None;
                column.not_null_identity = None;
            }
            column.not_null = true;
            column.not_null_is_local = true;
            column.not_null_validated = true;
        }
    }
}

/// The keys a partition clones from its parent, which `DefineRelation` creates before the table's CHECK constraints: each unique index cloned from the parent is checked against the table's own partition key, and each cloned key's index is named, a name the CHECKs the table inherits hold being taken, and takes its OIDs; then `CloneForeignKeyConstraints` creates the partition's copies of the parent's foreign keys. The namer goes on to name the declared keys.
pub fn clone_parent_keys<'a>(
    catalog: &'a dyn IndexNameCatalog,
    table: &mut CreateTable,
    inherited: &InheritedDefinitions,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<ConstraintIndexNamer<'a>, SQLError> {
    let mut indexes = ConstraintIndexNamer::new(catalog, &table.name)?;
    indexes.occupy(
        table
            .checks
            .iter()
            .filter(|check| !check.is_local)
            .filter_map(|check| check.name.clone()),
    );
    let partition = table.hierarchy.partition_spec.as_ref();
    for key in &mut table.key_constraints[..inherited.keys] {
        if let Some(partition) = partition {
            validate_partitioned_key_constraint(&table.name, key, partition)?;
        }
        indexes.name(key)?;
        materialize_key_identity(key, allocate).map_err(ConstraintMetadataError::into_sql_error)?;
    }
    for foreign_key in &mut table.foreign_keys[..inherited.foreign_keys] {
        materialize_foreign_key_identity(
            &mut foreign_key.object_id,
            &mut foreign_key.catalog_identity,
            allocate,
        )
        .map_err(ConstraintMetadataError::into_sql_error)?;
    }
    if let Some(partition) = partition {
        for keys in &inherited.unique_indexes {
            let columns = keys
                .iter()
                .map(crate::ast::IndexKey::column)
                .collect::<Vec<_>>();
            validate_partitioned_unique_key(
                &table.name,
                &PartitionedUniqueKey {
                    constraint_type: TableKeyConstraintKind::Unique,
                    columns: &columns,
                    without_overlaps: false,
                },
                partition,
            )?;
        }
    }
    Ok(indexes)
}

/// Define the keys the table declares as `DefineIndex` does once the table's CHECK and NOT NULL constraints exist: each key, primary key first, against an inherited primary key, the partition key and its index attributes. Each key's index is named after its checks, so a later key's errors follow an earlier key's name conflict, and no index takes a name a constraint of the table holds; the index then takes its OID and the constraint its own.
pub fn define_declared_keys(
    mut indexes: ConstraintIndexNamer<'_>,
    table: &mut CreateTable,
    inherited: &InheritedDefinitions,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    indexes.occupy(
        table
            .columns
            .iter()
            .flat_map(|column| [column.not_null_name.clone(), column.check_name.clone()])
            .chain(table.checks.iter().map(|check| check.name.clone()))
            .flatten(),
    );
    let inherited_primary_key = table.key_constraints[..inherited.keys]
        .iter()
        .any(|key| key.kind == TableKeyConstraintKind::PrimaryKey);
    let partition = table.hierarchy.partition_spec.as_ref();
    let declared = &mut table.key_constraints[inherited.keys..];
    for key in declared.iter_mut() {
        validate_key_definition(
            &KeyRelation {
                table: &table.name,
                columns: &table.columns,
                partition,
                has_primary_key: inherited_primary_key,
            },
            key,
        )?;
        indexes.name(key)?;
        materialize_key_identity(key, allocate).map_err(ConstraintMetadataError::into_sql_error)?;
    }
    mark_single_column_keys(&mut table.columns, &table.key_constraints[inherited.keys..]);
    Ok(())
}

/// Single-column keys also flag their columns for the scalar key paths that read the flags.
fn mark_single_column_keys(columns: &mut [ColumnDef], keys: &[TableKeyConstraint]) {
    for key in keys {
        let [name] = key.columns.as_slice() else {
            continue;
        };
        let Some(column) = columns.iter_mut().find(|column| column.name == *name) else {
            continue;
        };
        match key.kind {
            TableKeyConstraintKind::PrimaryKey => column.primary_key = true,
            TableKeyConstraintKind::Unique => column.unique = true,
        }
    }
}
