//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table authority reconstruction preserves ordered ACL paths and independent tuple revisions.

use super::{collections::ImageValue, expressions::payload, invalid, Fields};
use crate::{
    catalog::security::table_binding::{
        BoundTableAclEntry, BoundTableSecurity, CapturedTablePrivileges, CapturedTableSecurity,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    catalog_acl::RelationAclRevisions,
    image_io::{decode_image, AsyncImageInput},
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation, OwnedMap},
    CancellationToken,
};

impl Fields<'_> {
    async fn native_authority_map<T: ImageValue>(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<OwnedMap<String, T>, SQLError> {
        fn publish<T>(
            values: &mut OwnedMap<String, T>,
            entry: (Budgeted<String>, Budgeted<T>),
            memory: &mut MemoryReservation,
        ) -> Result<(), SQLError> {
            memory.grow(OwnedMap::<String, T>::entry_bytes())?;
            values.insert(payload(entry.0, memory), payload(entry.1, memory));
            Ok(())
        }
        let mut values = OwnedMap::new();
        let mut pending: Option<(Budgeted<String>, Budgeted<T>)> = None;
        for _ in 0..count {
            let name = self
                .ordered_name(pending.as_ref().map(|(name, _)| &**name))
                .await?;
            let value = T::read(self).await?;
            if let Some(previous) = pending.replace((name, value)) {
                publish(&mut values, previous, memory)?;
            }
        }
        if let Some(last) = pending {
            publish(&mut values, last, memory)?;
        }
        Ok(values)
    }

    async fn table_column_acls(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<(String, Vec<BoundTableAclEntry>)>, SQLError> {
        let mut values: BudgetedVec<(String, Vec<BoundTableAclEntry>)> =
            BudgetedVec::new(self.binary.memory);
        values.reserve(count)?;
        for _ in 0..count {
            let name = self
                .ordered_name(values.last().map(|(name, _)| name))
                .await?;
            let acl = self.acl_entries().await?;
            values.push((payload(name, memory), payload(acl, memory)))?;
        }
        let (values, reservation) = values.into_parts();
        memory.absorb(reservation);
        Ok(values)
    }

    async fn table_column_revisions(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<(String, [u8; 16])>, SQLError> {
        let mut values: BudgetedVec<(String, [u8; 16])> = BudgetedVec::new(self.binary.memory);
        values.reserve(count)?;
        for _ in 0..count {
            let name = self
                .ordered_name(values.last().map(|(name, _)| name))
                .await?;
            let revision = self.object_id().await?;
            values.push((payload(name, memory), revision))?;
        }
        let (values, reservation) = values.into_parts();
        memory.absorb(reservation);
        Ok(values)
    }
}

pub(in super::super) fn bound_table_security(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<BoundTableSecurity>, SQLError> {
    decode_image(reader, memory, cancellation, async |binary| {
        bound_table_security_from(binary).await
    })
}

pub(in super::super) async fn bound_table_security_from(
    binary: AsyncImageInput<'_>,
) -> Result<Budgeted<BoundTableSecurity>, SQLError> {
    let memory = binary.memory;
    let mut input = Fields {
        binary,
        retained_children: &mut 0,
    };
    if input.header(super::super::TABLE_SECURITY).await? != 1 {
        return Err(invalid("table authority count"));
    }
    let role_owner = input.identity().await?;
    let relation = input.optional_incarnation().await?;
    let acl_count = input.binary.count().await?;
    let revision_count = input.binary.count().await?;
    let mut reservation = memory.empty_reservation();
    let acl = payload(input.acl().await?, &mut reservation);
    let column_acls = input
        .native_authority_map(acl_count, &mut reservation)
        .await?;
    let columns = input
        .native_authority_map(revision_count, &mut reservation)
        .await?;
    input.binary.end().await?;
    Ok(Budgeted::new(
        BoundTableSecurity {
            role_owner,
            acl,
            column_acls,
            acl_revisions: RelationAclRevisions { relation, columns },
        },
        reservation,
    ))
}

pub(in super::super) fn table_security(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<CapturedTableSecurity>, SQLError> {
    decode_image(reader, memory, cancellation, async |binary| {
        table_security_from(binary).await
    })
}

pub(in super::super) async fn table_security_from(
    binary: AsyncImageInput<'_>,
) -> Result<Budgeted<CapturedTableSecurity>, SQLError> {
    let memory = binary.memory;
    let mut input = Fields {
        binary,
        retained_children: &mut 0,
    };
    if input.header(super::super::TABLE_SECURITY).await? != 1 {
        return Err(invalid("table authority count"));
    }
    let role_owner = input.identity().await?;
    let relation_revision = input.optional_incarnation().await?;
    let acl_count = input.binary.count().await?;
    let revision_count = input.binary.count().await?;
    let mut reservation = memory.empty_reservation();
    let acl = payload(input.acl().await?, &mut reservation);
    let column_acls = input.table_column_acls(acl_count, &mut reservation).await?;
    let column_revisions = input
        .table_column_revisions(revision_count, &mut reservation)
        .await?;
    input.binary.end().await?;
    Ok(Budgeted::new(
        CapturedTableSecurity {
            privileges: CapturedTablePrivileges::from_image_fields(role_owner, acl, column_acls),
            relation_revision,
            column_revisions,
        },
        reservation,
    ))
}

impl ImageValue for Vec<BoundTableAclEntry> {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        input.acl_entries().await
    }
}

impl ImageValue for [u8; 16] {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        Ok(Budgeted::new(
            input.object_id().await?,
            input.binary.memory.empty_reservation(),
        ))
    }
}
