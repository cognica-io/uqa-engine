//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private native constraints retain admitted values without publication or constraint execution.

use super::{
    invalid, payload, read_list, read_list_from, read_list_with_footprint, AsyncImageInput, Fields,
    ImageValue,
};
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
    read_list_with_footprint(reader, memory, cancellation, *b"UQATCK01")
}

/// Reconstruct native key metadata; relation-wide key/index validation belongs to the enclosing restore.
pub fn read_table_keys_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Vec<TableKeyConstraint>>, SQLError> {
    read_list(reader, memory, cancellation, *b"UQATKY01")
}

/// Reconstruct native foreign keys without selecting replacement indexes or resolving relations.
pub fn read_foreign_keys_image(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<Vec<ForeignKey>>, SQLError> {
    read_list(reader, memory, cancellation, *b"UQAFKY01")
}

impl Fields<'_> {
    async fn table_check(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<TableCheck, SQLError> {
        let name = payload(self.optional_text().await?, memory);
        let object_id = self.optional_incarnation().await?;
        let catalog_oid = self.optional_catalog_oid().await?;
        let is_local = self.flag().await?;
        let enforced = self.flag().await?;
        let validated = self.flag().await?;
        let no_inherit = self.flag().await?;
        let has_partition = self.flag().await?;
        let expr = payload(self.expression().await?, memory);
        let partition_constraint = if has_partition {
            Some(DetachedPartitionConstraint {
                spec: self.partition_spec(memory).await?,
                bound: self.partition_bound(memory).await?,
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

    pub(super) async fn table_key(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<TableKeyConstraint, SQLError> {
        let catalog_identity = self.optional_catalog_identity().await?;
        let index_identity = self.optional_catalog_identity().await?;
        let name = payload(self.optional_text().await?, memory);
        let kind = match self.binary.array::<1>().await?[0] {
            0 => TableKeyConstraintKind::PrimaryKey,
            1 => TableKeyConstraintKind::Unique,
            _ => return Err(invalid("table key kind")),
        };
        let nulls_not_distinct = self.flag().await?;
        let without_overlaps = self.flag().await?;
        let column_count = self.binary.count().await?;
        let included_count = self.binary.count().await?;
        let columns = self.texts(column_count, memory).await?;
        let included_columns = self.texts(included_count, memory).await?;
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

    pub(super) async fn table_foreign_key(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<ForeignKey, SQLError> {
        let referenced_index = self.optional_incarnation().await?;
        let referenced_key = payload(self.optional_text().await?, memory);
        let name = payload(self.optional_text().await?, memory);
        let object_id = self.optional_incarnation().await?;
        let catalog_identity = self.optional_catalog_identity().await?;
        let ref_table = payload(self.binary.text().await?, memory);
        let on_update = self.foreign_action().await?;
        let on_delete = self.foreign_action().await?;
        let match_type = self.foreign_match().await?;
        let enforced = self.flag().await?;
        let validated = self.flag().await?;
        let deferrable = self.flag().await?;
        let initially_deferred = self.flag().await?;
        let period = self.flag().await?;
        let local_count = self.binary.count().await?;
        let ref_count = self.binary.count().await?;
        let set_count = self.binary.count().await?;
        let partition_count = self.binary.count().await?;
        let local_columns = self.texts(local_count, memory).await?;
        let ref_columns = self.texts(ref_count, memory).await?;
        let on_delete_set_columns = self.texts(set_count, memory).await?;
        let referenced_partitions = payload(
            self.image_values::<crate::ast::ReferencedPartitionConstraint>(partition_count)
                .await?,
            memory,
        );
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

impl ImageValue for TableCheck {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut memory = input.binary.memory.empty_reservation();
        let value = input.table_check(&mut memory).await?;
        Ok(Budgeted::new(value, memory))
    }
}

impl ImageValue for TableKeyConstraint {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut memory = input.binary.memory.empty_reservation();
        let value = input.table_key(&mut memory).await?;
        Ok(Budgeted::new(value, memory))
    }
}

impl ImageValue for ForeignKey {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut memory = input.binary.memory.empty_reservation();
        let value = input.table_foreign_key(&mut memory).await?;
        Ok(Budgeted::new(value, memory))
    }
}

/// Resume CHECK and detached-partition metadata with the original retained literal footprint.
pub async fn read_table_checks_image_from(
    input: AsyncImageInput<'_>,
) -> Result<(Budgeted<Vec<TableCheck>>, usize), SQLError> {
    read_list_from(input, *b"UQATCK01").await
}

/// Resume ordered foreign keys on the enclosing native input without rebinding references.
pub async fn read_foreign_keys_image_from(
    input: AsyncImageInput<'_>,
) -> Result<Budgeted<Vec<ForeignKey>>, SQLError> {
    Ok(read_list_from(input, *b"UQAFKY01").await?.0)
}

/// Resume ordered key declarations on the enclosing native input without selecting indexes.
pub async fn read_table_keys_image_from(
    input: AsyncImageInput<'_>,
) -> Result<Budgeted<Vec<TableKeyConstraint>>, SQLError> {
    Ok(read_list_from(input, *b"UQATKY01").await?.0)
}
