//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table authority reconstruction preserves ordered ACL paths and independent tuple revisions.

use super::{expressions::payload, invalid, Input};
use crate::{
    catalog::security::table_binding::{
        BoundTableAclEntry, CapturedTablePrivileges, CapturedTableSecurity,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken,
};

impl Input<'_> {
    fn table_column_acls(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<(String, Vec<BoundTableAclEntry>)>, SQLError> {
        let mut values: BudgetedVec<(String, Vec<BoundTableAclEntry>)> =
            BudgetedVec::new(self.binary.memory);
        values.reserve(count)?;
        for _ in 0..count {
            let name = self.ordered_name(values.last().map(|(name, _)| name))?;
            let acl = self.acl_entries()?;
            values.push((payload(name, memory), payload(acl, memory)))?;
        }
        let (values, reservation) = values.into_parts();
        memory.absorb(reservation);
        Ok(values)
    }

    fn table_column_revisions(
        &mut self,
        count: usize,
        memory: &mut MemoryReservation,
    ) -> Result<Vec<(String, [u8; 16])>, SQLError> {
        let mut values: BudgetedVec<(String, [u8; 16])> = BudgetedVec::new(self.binary.memory);
        values.reserve(count)?;
        for _ in 0..count {
            let name = self.ordered_name(values.last().map(|(name, _)| name))?;
            let revision = self.object_id()?;
            values.push((payload(name, memory), revision))?;
        }
        let (values, reservation) = values.into_parts();
        memory.absorb(reservation);
        Ok(values)
    }
}

pub(in super::super) fn table_security(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Budgeted<CapturedTableSecurity>, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    if input.header(super::super::TABLE_SECURITY)? != 1 {
        return Err(invalid("table authority count"));
    }
    let role_owner = input.identity()?;
    let relation_revision = input.optional_incarnation()?;
    let acl_count = input.binary.count()?;
    let revision_count = input.binary.count()?;
    let mut reservation = memory.empty_reservation();
    let acl = payload(input.acl()?, &mut reservation);
    let column_acls = input.table_column_acls(acl_count, &mut reservation)?;
    let column_revisions = input.table_column_revisions(revision_count, &mut reservation)?;
    input.binary.end()?;
    Ok(Budgeted::new(
        CapturedTableSecurity {
            privileges: CapturedTablePrivileges::from_image_fields(role_owner, acl, column_acls),
            relation_revision,
            column_revisions,
        },
        reservation,
    ))
}
