//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original foreign-key endpoints and partition-derived rows use borrowed declaration fields.

use super::{
    bound_relation_identity_with_control, invalid, BudgetedMap, Catalog, ColumnDef, ForeignKey,
    Index, NativeConstraintTable, SQLError,
};
use crate::ast::{ColumnType, ForeignKeyAction, ForeignKeyRef, ReferencedPartitionConstraint};

pub(super) struct Reference<'a> {
    local: &'a [String],
    target: &'a str,
    columns: &'a [String],
    index: Option<[u8; 16]>,
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
            period: key.period,
            on_update: key.on_update,
            on_delete: key.on_delete,
            set_columns: &key.on_delete_set_columns,
            partitions: &key.referenced_partitions,
        }
    }
}

impl<'a> Catalog<'a> {
    pub(super) fn foreign_key(
        &self,
        local: NativeConstraintTable<'a>,
        key: Reference<'_>,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        if local.hierarchy.is_none() || key.local.is_empty() || key.local.len() != key.columns.len()
        {
            return Err(invalid("foreign-key relation or column arity"));
        }
        let name = bound_relation_identity_with_control(key.target, &self.control)?;
        let target = *self
            .tables
            .get(&*name)
            .ok_or_else(|| invalid("missing foreign-key table"))?;
        if target.hierarchy.is_none() {
            return Err(invalid("foreign-key target is not an ordinary table"));
        }
        self.column_list(local, key.local, false)?;
        self.column_list(target, key.columns, true)?;
        for (source, destination) in key.local.iter().zip(key.columns) {
            let source = self.column(local, source)?;
            let destination = self.column(target, destination)?;
            crate::type_resolution::foreign_key_operand_type_with_control(
                &source.ty,
                &destination.ty,
                &self.control,
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
            bound_relation_identity_with_control(&index.row.table_name, &self.control)?;
        if *index_table != *target.name
            || address.table_object_id != target.object_id
            || !address.identity.is_valid()
            || !index.metadata.unique
            || index.metadata.predicate.is_some()
        {
            return Err(invalid("foreign-key index incarnation or eligibility"));
        }
        self.index_columns(index, key.columns)?;
        if key.period {
            self.period(local, target, &key, index)?;
        }
        if !key.set_columns.is_empty() {
            if !matches!(
                key.on_delete,
                ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault
            ) {
                return Err(invalid("foreign-key SET column action"));
            }
            self.column_list(local, key.set_columns, false)?;
            for column in key.set_columns {
                self.control.check()?;
                if !key.local.contains(column) {
                    return Err(invalid("foreign-key SET column membership"));
                }
            }
        }
        self.referenced_partitions(target, key.partitions)
    }

    fn index_columns(&self, index: &Index<'_>, columns: &[String]) -> Result<(), SQLError> {
        let mut selected = BudgetedMap::new(self.memory);
        for column in columns {
            self.control.check()?;
            selected.insert(column.as_str(), false)?;
        }
        let mut count = 0;
        let ordinary = crate::catalog::index::stored::input::visit_column_keys(
            &index.row.columns_json,
            &self.control,
            |name| {
                let seen = selected
                    .get_mut(name)
                    .ok_or_else(|| invalid("foreign-key index columns"))?;
                if std::mem::replace(seen, true) {
                    return Err(invalid("duplicate foreign-key index column"));
                }
                count += 1;
                Ok(())
            },
        )?;
        if !ordinary || count != columns.len() {
            return Err(invalid("foreign-key index expressions or arity"));
        }
        Ok(())
    }

    fn period(
        &self,
        local: NativeConstraintTable<'a>,
        target: NativeConstraintTable<'a>,
        key: &Reference<'_>,
        index: &Index<'_>,
    ) -> Result<(), SQLError> {
        if key.local.len() < 2
            || key.on_update != ForeignKeyAction::NoAction
            || key.on_delete != ForeignKeyAction::NoAction
        {
            return Err(invalid("PERIOD foreign-key shape or action"));
        }
        let source = &self
            .column(local, key.local.last().expect("nonempty key"))?
            .ty;
        let destination = &self
            .column(target, key.columns.last().expect("matching key arity"))?
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
            self.control.check()?;
            if candidate
                .catalog_identity
                .is_some_and(|id| id.object_id == owner)
                && candidate.without_overlaps
                && candidate.columns == key.columns
            {
                return Ok(());
            }
        }
        Err(invalid("PERIOD foreign-key referenced key"))
    }

    fn referenced_partitions(
        &self,
        target: NativeConstraintTable<'a>,
        partitions: &[ReferencedPartitionConstraint],
    ) -> Result<(), SQLError> {
        let mut seen = BudgetedMap::new(self.memory);
        for derived in partitions {
            self.control.check()?;
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
            let child = self
                .identities
                .get(&derived.partition)
                .ok_or_else(|| invalid("missing referenced partition"))?;
            let hierarchy = child
                .hierarchy
                .ok_or_else(|| invalid("referenced partition relation kind"))?;
            if !hierarchy.is_partition() || hierarchy.parents.len() != 1 {
                return Err(invalid("referenced partition edge"));
            }
            let name = bound_relation_identity_with_control(&hierarchy.parents[0], &self.control)?;
            let parent = self
                .tables
                .get(&*name)
                .ok_or_else(|| invalid("missing referenced partition parent"))?;
            if parent.object_id != expected
                || parent
                    .hierarchy
                    .is_none_or(|hierarchy| hierarchy.partition_spec.is_none())
            {
                return Err(invalid("referenced partition parent incarnation"));
            }
        }
        Ok(())
    }
}
