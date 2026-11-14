//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Membership decoding retains original bindings and verifies their explicit ordered keys.

use super::{invalid, Fields};
use crate::{
    catalog::roles::{RoleMembership, RoleMembershipKey, RoleMemberships},
    SQLError,
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, BudgetedMap, MemoryBudget},
    CancellationToken,
};

pub(in super::super) fn memberships(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<RoleMemberships, SQLError> {
    super::continuation::read_registry(reader, memory, cancellation)
}

impl RoleMemberships {
    /// Retain native fields and claims across independent entry, byte and poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry::<Self>(memory, mutations, cancellation)
    }
}

impl super::continuation::CatalogEntries for RoleMemberships {
    type Scratch = BudgetedMap<i64, ()>;
    const MAGIC: [u8; 8] = super::super::MEMBERSHIPS;
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (Self::with_budget(memory), BudgetedMap::new(memory))
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        identities: &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let key = RoleMembershipKey {
            role: input.identity().await?,
            member: input.identity().await?,
            grantor: input.identity().await?,
        };
        if result
            .last_durable_key()
            .is_some_and(|previous| *previous >= key)
        {
            return Err(invalid("membership order"));
        }
        let oid = input.oid().await?;
        let role = input.binding().await?;
        let member = input.binding().await?;
        let grantor = input.binding().await?;
        let [bits] = input.binary.array().await?;
        if bits & !7 != 0 {
            return Err(invalid("membership option bits"));
        }
        if identities.insert(oid, ())?.is_some() {
            return Err(invalid("duplicate membership OID"));
        }
        let (role, mut reservation) = role.into_parts();
        let (member, member_memory) = member.into_parts();
        reservation.absorb(member_memory);
        let (grantor, grantor_memory) = grantor.into_parts();
        reservation.absorb(grantor_memory);
        let membership = Budgeted::new(
            RoleMembership {
                oid,
                role,
                member,
                grantor,
                admin_option: bits & 1 != 0,
                inherit_option: bits & 2 != 0,
                set_option: bits & 4 != 0,
            },
            reservation,
        );
        if membership.key() != key {
            return Err(invalid("membership key"));
        }
        result.insert_image_entry(
            Budgeted::new(key, input.binary.memory.empty_reservation()),
            membership,
        )?;

        Ok(())
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}
