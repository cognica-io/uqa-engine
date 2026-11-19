//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role definitions preserve native keys, tuple revisions and explicit attribute bits.

use super::{invalid, validate_identity, Fields};
use crate::{
    ast::RoleAttribute,
    catalog::roles::{RoleDefinition, RoleDefinitions, RoleIdentity},
    SQLError,
};
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    memory::{Budgeted, BudgetedMap, MemoryBudget, OwnedSet},
    CancellationToken,
};

pub(in super::super) fn roles(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<RoleDefinitions, SQLError> {
    super::continuation::read_registry(reader, memory, cancellation)
}

impl RoleDefinitions {
    /// Retain native entries and identity claims between calls; future mutations use the destination allowance.
    pub fn image_input(
        input: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry::<Self>(input, mutations, cancellation)
    }
}

impl super::continuation::CatalogEntries for RoleDefinitions {
    type Scratch = (BudgetedMap<i64, ()>, BudgetedMap<[u8; 16], ()>);
    const MAGIC: [u8; 8] = super::super::ROLES;
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (
            Self::with_budget(memory),
            (BudgetedMap::new(memory), BudgetedMap::new(memory)),
        )
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        scratch: &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let (oids, identities) = scratch;
        let key = input.ordered_name(result.last_durable_key()).await?;
        let oid = input.oid().await?;
        let object_id = input.binary.array().await?;
        validate_identity(RoleIdentity { oid, object_id })?;
        let revision = u64::from_le_bytes(input.binary.array().await?);
        if revision == 0 {
            return Err(invalid("role revision"));
        }
        let name = input.binary.text().await?;
        if input.compare_text(&name, &key).await? != Ordering::Equal {
            return Err(invalid("role definition key"));
        }
        let [bits] = input.binary.array().await?;
        if bits & !127 != 0 {
            return Err(invalid("role attribute bits"));
        }
        let connection_limit = i32::from_le_bytes(input.binary.array().await?);
        if connection_limit < -1 {
            return Err(invalid("connection limit"));
        }
        if oids.insert(oid, ())?.is_some() || identities.insert(object_id, ())?.is_some() {
            return Err(invalid("duplicate role identity"));
        }
        let attribute_memory = input
            .binary
            .memory
            .reserve(bits.count_ones() as usize * OwnedSet::<RoleAttribute>::entry_bytes())?;
        let mut attributes = OwnedSet::new();
        for attribute in [
            RoleAttribute::Superuser,
            RoleAttribute::Inherit,
            RoleAttribute::CreateRole,
            RoleAttribute::CreateDb,
            RoleAttribute::Login,
            RoleAttribute::Replication,
            RoleAttribute::BypassRls,
        ] {
            if bits & super::super::fields::attribute_bit(attribute) != 0 {
                attributes.insert(attribute);
            }
        }
        let (name, mut reservation) = name.into_parts();
        reservation.absorb(attribute_memory);
        result.insert_image_entry(
            key,
            Budgeted::new(
                RoleDefinition {
                    oid,
                    object_id,
                    revision,
                    name,
                    attributes,
                    connection_limit,
                },
                reservation,
            ),
        )?;
        Ok(())
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}
