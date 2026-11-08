//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private native constraints retain admitted values without publication or constraint execution.

use super::{invalid, payload, read_list, read_list_with_footprint, Input};
use crate::{
    ast::{
        DetachedPartitionConstraint, ForeignKey, TableCheck, TableKeyConstraint,
        TableKeyConstraintKind,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, MemoryBudget, MemoryReservation},
    CancellationToken,
};

/// Reconstruct ordered CHECK declarations and original detached partition constraints.
pub fn read_table_checks_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Vec<TableCheck>>, SQLError> {
    Ok(read_table_checks_image_with_footprint(reader, memory, cancellation)?.0)
}

/// Reconstruct native checks and report separately retained child bytes for independent capture admission.
pub fn read_table_checks_image_with_footprint(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(Budgeted<Vec<TableCheck>>, usize), SQLError> {
    read_list_with_footprint(
        reader,
        memory,
        cancellation,
        *b"UQATCK01",
        Input::table_check,
    )
}

/// Reconstruct native key metadata; relation-wide key/index validation belongs to the enclosing restore.
pub fn read_table_keys_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Vec<TableKeyConstraint>>, SQLError> {
    read_list(reader, memory, cancellation, *b"UQATKY01", Input::table_key)
}

/// Reconstruct native foreign keys without selecting replacement indexes or resolving relations.
pub fn read_foreign_keys_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Vec<ForeignKey>>, SQLError> {
    read_list(
        reader,
        memory,
        cancellation,
        *b"UQAFKY01",
        Input::table_foreign_key,
    )
}

impl Input<'_> {
    fn table_check(&mut self, memory: &mut MemoryReservation) -> Result<TableCheck, SQLError> {
        let name = payload(self.optional_text()?, memory);
        let object_id = self.optional_incarnation()?;
        let catalog_oid = self.optional_catalog_oid()?;
        let is_local = self.flag()?;
        let enforced = self.flag()?;
        let validated = self.flag()?;
        let no_inherit = self.flag()?;
        let has_partition = self.flag()?;
        let expr = payload(self.expression()?, memory);
        let partition_constraint = if has_partition {
            Some(DetachedPartitionConstraint {
                spec: self.partition_spec(memory)?,
                bound: self.partition_bound(memory)?,
            })
        } else {
            None
        };
        Ok(TableCheck {
            name,
            object_id,
            catalog_oid,
            is_local,
            expr,
            enforced,
            validated,
            no_inherit,
            partition_constraint,
        })
    }

    pub(super) fn table_key(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<TableKeyConstraint, SQLError> {
        let catalog_identity = self.optional_catalog_identity()?;
        let index_identity = self.optional_catalog_identity()?;
        let name = payload(self.optional_text()?, memory);
        let kind = match self.binary.array::<1>()?[0] {
            0 => TableKeyConstraintKind::PrimaryKey,
            1 => TableKeyConstraintKind::Unique,
            _ => return Err(invalid("table key kind")),
        };
        let nulls_not_distinct = self.flag()?;
        let without_overlaps = self.flag()?;
        let column_count = self.binary.count()?;
        let included_count = self.binary.count()?;
        let columns = self.texts(column_count, memory)?;
        let included_columns = self.texts(included_count, memory)?;
        Ok(TableKeyConstraint {
            catalog_identity,
            index_identity,
            name,
            kind,
            columns,
            included_columns,
            nulls_not_distinct,
            without_overlaps,
        })
    }

    pub(super) fn table_foreign_key(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<ForeignKey, SQLError> {
        let referenced_index = self.optional_incarnation()?;
        let referenced_key = payload(self.optional_text()?, memory);
        let name = payload(self.optional_text()?, memory);
        let object_id = self.optional_incarnation()?;
        let catalog_identity = self.optional_catalog_identity()?;
        let ref_table = payload(self.binary.text()?, memory);
        let on_update = self.foreign_action()?;
        let on_delete = self.foreign_action()?;
        let match_type = self.foreign_match()?;
        let enforced = self.flag()?;
        let validated = self.flag()?;
        let deferrable = self.flag()?;
        let initially_deferred = self.flag()?;
        let period = self.flag()?;
        let local_count = self.binary.count()?;
        let ref_count = self.binary.count()?;
        let set_count = self.binary.count()?;
        let partition_count = self.binary.count()?;
        let local_columns = self.texts(local_count, memory)?;
        let ref_columns = self.texts(ref_count, memory)?;
        let on_delete_set_columns = self.texts(set_count, memory)?;
        let referenced_partitions =
            self.items(partition_count, memory, Self::referenced_partition)?;
        Ok(ForeignKey {
            referenced_index,
            referenced_key,
            name,
            object_id,
            catalog_identity,
            local_columns,
            ref_table,
            ref_columns,
            on_update,
            on_delete,
            on_delete_set_columns,
            match_type,
            enforced,
            validated,
            deferrable,
            initially_deferred,
            period,
            referenced_partitions,
        })
    }
}
