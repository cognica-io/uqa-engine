//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original foreign-key endpoints and partition-derived rows use borrowed declaration fields.

use super::partitions::Partitions;
use super::{
    bound_relation_identity_with_work, invalid, retire_entries, Budgeted, BudgetedMap, Catalog,
    CatalogWork, ColumnDef, ForeignKey, Index, NativeConstraintTable, OwnedMap, ReferenceWork,
    SQLError,
};
use crate::ast::{ColumnType, ForeignKeyAction, ForeignKeyRef, ReferencedPartitionConstraint};

use crate::catalog::{
    index::stored::input::{ColumnKeys, StoredColumnKey},
    work::compare_text,
};
use crate::schema::inheritance::foreign_keys::attachment::same_columns;
use uqa_core::memory::MapKeyInput;

pub(super) struct Reference<'a> {
    local: &'a [String],
    target: &'a str,
    columns: &'a [String],
    index: Option<[u8; 16]>,
    family: Option<[u8; 16]>,
    period: bool,
    on_update: ForeignKeyAction,
    on_delete: ForeignKeyAction,
    set_columns: &'a [String],
    partitions: &'a [ReferencedPartitionConstraint],
}

impl<'a> Reference<'a> {
    pub(super) fn column(column: &'a ColumnDef, reference: &'a ForeignKeyRef) -> Self {
        Self {
            local: std::slice::from_ref(&column.name),
            target: &reference.table,
            columns: reference.column.as_slice(),
            index: reference.referenced_index,
            family: reference.object_id,
            period: reference.period,
            on_update: reference.on_update,
            on_delete: reference.on_delete,
            set_columns: &[],
            partitions: &reference.referenced_partitions,
        }
    }

    pub(super) fn table(key: &'a ForeignKey) -> Self {
        Self {
            local: &key.local_columns,
            target: &key.ref_table,
            columns: &key.ref_columns,
            index: key.referenced_index,
            family: key.object_id,
            period: key.period,
            on_update: key.on_update,
            on_delete: key.on_delete,
            set_columns: &key.on_delete_set_columns,
            partitions: &key.referenced_partitions,
        }
    }
}

impl<'a> Catalog<'a> {
    pub(super) async fn foreign_key(
        &self,
        local: NativeConstraintTable<'a>,
        key: Reference<'_>,
        partitions: &Partitions<'_>,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        input.checkpoint().await?;
        if local.hierarchy.is_none() || key.local.is_empty() || key.local.len() != key.columns.len()
        {
            return Err(invalid("foreign-key relation or column arity"));
        }
        let name = bound_relation_identity_with_work(key.target, input.control, input.work).await?;
        let target = *self
            .tables
            .get_from(&*name, input)
            .await?
            .ok_or_else(|| invalid("missing foreign-key table"))?;
        if target.hierarchy.is_none() {
            return Err(invalid("foreign-key target is not an ordinary table"));
        }
        self.column_list(local, key.local, false, input).await?;
        self.column_list(target, key.columns, true, input).await?;
        for (source, destination) in key.local.iter().zip(key.columns) {
            let source = self.column(local, source, input).await?;
            let destination = self.column(target, destination, input).await?;
            crate::type_resolution::foreign_key_operand_type_with_control(
                &source.ty,
                &destination.ty,
                input.control,
            )
            .map_err(|error| match error.sqlstate() {
                Some("53200" | "57014") => error,
                _ => invalid("incompatible foreign-key column types"),
            })?;
        }
        let index = key
            .index
            .and_then(|identity| self.indexes.get(&identity))
            .ok_or_else(|| invalid("missing original foreign-key index"))?;
        let address = index.metadata.catalog.as_ref().expect("addressed index");
        let index_table =
            bound_relation_identity_with_work(&index.row.table_name, input.control, input.work)
                .await?;
        if input.compare(&*index_table, &target.name).await?.is_ne()
            || address.table_object_id != target.object_id
            || !address.identity.is_valid()
            || !index.metadata.unique
            || index.metadata.predicate.is_some()
        {
            return Err(invalid("foreign-key index incarnation or eligibility"));
        }
        self.index_columns(index, key.columns, input).await?;
        if key.period {
            self.period(local, target, &key, index, input).await?;
        }
        if !key.set_columns.is_empty() {
            if !matches!(
                key.on_delete,
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault
            ) {
                return Err(invalid("foreign-key SET column action"));
            }
            self.column_list(local, key.set_columns, false, input)
                .await?;
            for column in key.set_columns {
                input.checkpoint().await?;
                let mut matched = false;
                for name in key.local {
                    input.checkpoint().await?;
                    if compare_text(input.work, name, column, input.control)
                        .await?
                        .is_eq()
                    {
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    return Err(invalid("foreign-key SET column membership"));
                }
            }
        }
        self.referenced_partitions(local, target, &key, partitions, input)
            .await
    }

    async fn index_columns(
        &self,
        index: &Index<'_>,
        columns: &[String],
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        let mut selected = OwnedMap::new();
        for column in columns {
            let name = Budgeted::new(column.as_str(), self.memory.reserve(0)?);
            selected
                .insert_admitted_key_from(name, false, input)
                .await?;
        }
        let mut keys = ColumnKeys::new(&index.row.columns_json, input.control, input.work).await?;
        let mut ordinary = true;
        let mut count = 0;
        while let Some(key) = keys.next(input.work).await? {
            let StoredColumnKey::Column(name) = key else {
                ordinary = false;
                continue;
            };
            let seen = selected
                .get_mut_from(name.as_str(), input)
                .await?
                .ok_or_else(|| invalid("foreign-key index columns"))?;
            if std::mem::replace(seen, true) {
                return Err(invalid("duplicate foreign-key index column"));
            }
            count += 1;
        }
        if !ordinary || count != columns.len() {
            return Err(invalid("foreign-key index expressions or arity"));
        }
        retire_entries(selected.into_iter(), input.control, input.work).await
    }

    async fn period(
        &self,
        local: NativeConstraintTable<'a>,
        target: NativeConstraintTable<'a>,
        key: &Reference<'_>,
        index: &Index<'_>,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        if key.local.len() < 2
            || key.on_update != ForeignKeyAction::NoAction
            || key.on_delete != ForeignKeyAction::NoAction
        {
            return Err(invalid("PERIOD foreign-key shape or action"));
        }
        let source = &self
            .column(local, key.local.last().expect("nonempty key"), input)
            .await?
            .ty;
        let destination = &self
            .column(
                target,
                key.columns.last().expect("matching key arity"),
                input,
            )
            .await?
            .ty;
        if !matches!(source, ColumnType::Range(_) | ColumnType::Multirange(_))
            || source != destination
        {
            return Err(invalid("PERIOD foreign-key column types"));
        }
        let owner = index
            .metadata
            .relationships
            .owning_constraint
            .ok_or_else(|| invalid("PERIOD foreign-key index has no owning constraint"))?;
        for candidate in target.keys {
            input.checkpoint().await?;
            if candidate
                .catalog_identity
                .is_some_and(|id| id.object_id == owner)
                && candidate.without_overlaps
                && same_columns(&candidate.columns, key.columns, input).await?
            {
                return Ok(());
            }
        }
        Err(invalid("PERIOD foreign-key referenced key"))
    }

    async fn referenced_partitions(
        &self,
        local: NativeConstraintTable<'a>,
        target: NativeConstraintTable<'a>,
        key: &Reference<'_>,
        partitions: &Partitions<'_>,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        if partitions.inherits_family(&local.object_id, key.family) {
            return if key.partitions.is_empty() {
                Ok(())
            } else {
                Err(invalid(
                    "inherited foreign key retains derived partition rows",
                ))
            };
        }
        let mut seen = BudgetedMap::new(self.memory);
        for derived in key.partitions {
            input.checkpoint().await?;
            if !derived.catalog_identity.is_valid()
                || derived.partition == target.object_id
                || seen.insert(derived.partition, ())?.is_some()
            {
                return Err(invalid("referenced partition identity"));
            }
            let expected = match derived.parent {
                Some(parent) if parent != derived.partition && seen.contains_key(&parent) => parent,
                Some(_) => return Err(invalid("referenced partition parent order")),
                None => target.object_id,
            };
            if partitions.parent(&derived.partition) != Some(expected) {
                return Err(invalid("referenced partition parent incarnation"));
            }
        }
        partitions.complete(target.object_id, &seen, input).await?;
        retire_entries(seen.into_entries(), input.control, input.work).await
    }
}
