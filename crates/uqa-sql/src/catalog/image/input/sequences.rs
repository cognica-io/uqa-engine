//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence authority images retain exact persistence and bound grants without resolving roles.

use super::{invalid, Fields};
use crate::{
    ast::RelationPersistence,
    catalog::{
        security::{sequence_registry::SequenceSecurities, BoundSequenceSecurity},
        temporary_namespace::is_temporary_schema_name,
    },
    SQLError,
};
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    memory::{Budgeted, MemoryBudget},
    CancellationToken, RelationIdentity,
};

pub(in super::super) fn sequence_authorities(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<SequenceSecurities, SQLError> {
    super::continuation::read_registry(reader, memory, cancellation)
}

impl SequenceSecurities {
    /// Retain native fields and claims across independent entry, byte and poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry::<Self>(memory, mutations, cancellation)
    }
}

impl super::continuation::CatalogEntries for SequenceSecurities {
    type Scratch = ();
    const MAGIC: [u8; 8] = super::super::SEQUENCE_AUTHORITIES;
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (Self::with_budget(memory), ())
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        (): &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let schema = input.binary.text().await?;
        let name = input.binary.text().await?;
        if schema.is_empty()
            || name.is_empty()
            || schema.contains('\0')
            || name.contains('\0')
            || is_temporary_schema_name(&schema)
        {
            return Err(invalid("sequence relation name"));
        }
        if let Some(previous) = result.last_durable_key() {
            let mut order = input.compare_text(&previous.schema, &schema).await?;
            if order == Ordering::Equal {
                order = input.compare_text(&previous.name, &name).await?;
            }
            if order != Ordering::Less {
                return Err(invalid("sequence authority order"));
            }
        }
        let persistence = match input.binary.array::<1>().await?[0] {
            0 => RelationPersistence::Permanent,
            1 => RelationPersistence::Unlogged,
            _ => return Err(invalid("sequence persistence")),
        };
        let role_owner = input.identity().await?;
        let (acl, acl_memory) = input.acl().await?.into_parts();
        let (schema, mut key_memory) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        key_memory.absorb(name_memory);
        result.insert_image_entry(
            Budgeted::new(RelationIdentity { schema, name }, key_memory),
            persistence,
            Budgeted::new(BoundSequenceSecurity { role_owner, acl }, acl_memory),
        )?;

        Ok(())
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}
