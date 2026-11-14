//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored table fields keep separate reservations through name binding and native publication.

use super::{MemoryBudget, Produced, ProductionControl, TableDefinition};
use crate::{
    ast::{
        read_stored_table_constraint_fields_json, ForeignKey, OnCommitAction,
        ParsedTableConstraints, RelationPersistence, TableCheck, TableConstraintSet,
        TableHierarchy, TableKeyConstraint,
    },
    catalog::{
        relation_attributes::DroppedAttribute,
        relation_oids::RelationCatalogOids,
        table_metadata::{DecodedTableMetadataValue, TableMetadata},
    },
    SQLError,
};
use uqa_core::{memory::Budgeted, CancellationToken};

/// A key's position within the original table declaration or its inherited partition metadata.
#[derive(Clone, Copy, Debug)]
pub enum StoredKeyLocation {
    Declared(usize),
    Inherited(usize),
}

/// Complete unpublished syntax with each original field's allowance. Borrowed validation and current-name selection precede consuming publication.
#[derive(Debug)]
pub struct StoredTableConstraints {
    parsed: ParsedTableConstraints,
    memory: MemoryBudget,
    original: CancellationToken,
    invoking: CancellationToken,
}

/// Independently retained native fields, ready for the surrounding table's atomic publication.
#[derive(Debug)]
pub struct NativeTableConstraints {
    pub dropped_attributes: TableMetadata<Vec<DroppedAttribute>>,
    pub columns_declared: Option<bool>,
    pub checks: TableDefinition<Vec<TableCheck>>,
    pub foreign_keys: TableMetadata<Vec<ForeignKey>>,
    pub key_constraints: TableMetadata<Vec<TableKeyConstraint>>,
    pub persistence: RelationPersistence,
    pub on_commit: OnCommitAction,
    pub hierarchy: TableDefinition<TableHierarchy>,
    pub catalog_oids: Option<RelationCatalogOids>,
    pub row_type_array_name: TableMetadata<Option<String>>,
}

impl StoredTableConstraints {
    pub fn read_stored_json(
        text: &str,
        memory: &MemoryBudget,
        original: &CancellationToken,
        invoking: &CancellationToken,
    ) -> Result<Self, SQLError> {
        let parsed = read_stored_table_constraint_fields_json(text, memory, original, invoking)?;
        Ok(Self {
            parsed,
            memory: memory.clone(),
            original: original.clone(),
            invoking: invoking.clone(),
        })
    }

    /// Replace only a key name, retaining its original structural identity and syntax. The new string must carry exactly its native capacity under this input's allowance. All rejection precedes mutation; name selection remains with the catalog owner.
    pub fn replace_key_name(
        &mut self,
        location: StoredKeyLocation,
        name: Produced<String>,
    ) -> Result<(), SQLError> {
        ProductionControl::new(&self.memory, &self.original, &self.invoking).check()?;
        if !name.is_retained_by(&self.memory) || name.reserved_bytes() != name.capacity() {
            return Err(SQLError::Internal(
                "stored key name requires its original exact payload allowance".into(),
            ));
        }
        let (key, memory) = match location {
            StoredKeyLocation::Declared(index) => (
                self.parsed.value.key_constraints.get_mut(index),
                &mut self.parsed.memory.key_constraints,
            ),
            StoredKeyLocation::Inherited(index) => (
                self.parsed
                    .value
                    .hierarchy
                    .partition_inherited_key_constraints
                    .get_mut(index),
                &mut self.parsed.memory.hierarchy,
            ),
        };
        let key = key.ok_or_else(|| SQLError::Internal("missing stored key position".into()))?;
        let (name, owned) = name
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("original stored name allowance"))
            .into_parts();
        memory.absorb(owned);
        let previous = key.name.replace(name);
        let previous_bytes = previous.as_ref().map_or(0, String::capacity);
        drop(previous);
        drop(memory.split(previous_bytes));
        Ok(())
    }

    /// Consume the complete declaration into independent native field owners. Only shared headers are allocated; payloads keep their original buffers and allowances. Failure retires every unpublished field.
    pub fn into_native(self) -> Result<NativeTableConstraints, SQLError> {
        let control = ProductionControl::new(&self.memory, &self.original, &self.invoking);
        control.check()?;
        let result = NativeTableConstraints {
            dropped_attributes: metadata(
                Budgeted::new(
                    self.parsed.value.dropped_attributes,
                    self.parsed.memory.dropped_attributes,
                ),
                &control,
                &self.original,
            )?,
            columns_declared: self.parsed.value.columns_declared,
            checks: TableDefinition::from_decoded_parts(
                Budgeted::new(self.parsed.value.checks, self.parsed.memory.checks),
                0,
                &control,
            )?,
            foreign_keys: metadata(
                Budgeted::new(
                    self.parsed.value.foreign_keys,
                    self.parsed.memory.foreign_keys,
                ),
                &control,
                &self.original,
            )?,
            key_constraints: metadata(
                Budgeted::new(
                    self.parsed.value.key_constraints,
                    self.parsed.memory.key_constraints,
                ),
                &control,
                &self.original,
            )?,
            persistence: self.parsed.value.persistence,
            on_commit: self.parsed.value.on_commit,
            hierarchy: TableDefinition::from_decoded_parts(
                Budgeted::new(self.parsed.value.hierarchy, self.parsed.memory.hierarchy),
                0,
                &control,
            )?,
            catalog_oids: self.parsed.value.catalog_oids,
            row_type_array_name: metadata(
                Budgeted::new(
                    self.parsed.value.row_type_array_name,
                    self.parsed.memory.row_type_array_name,
                ),
                &control,
                &self.original,
            )?,
        };
        control.check()?;
        Ok(result)
    }
}

impl std::ops::Deref for StoredTableConstraints {
    type Target = TableConstraintSet;
    fn deref(&self) -> &Self::Target {
        &self.parsed.value
    }
}

fn metadata<T: DecodedTableMetadataValue>(
    value: Budgeted<T>,
    control: &ProductionControl<'_>,
    original: &CancellationToken,
) -> Result<TableMetadata<T>, SQLError> {
    control.check()?;
    let result = TableMetadata::from_decoded(value, original)?;
    control.check()?;
    Ok(result)
}

#[cfg(test)]
mod tests;
