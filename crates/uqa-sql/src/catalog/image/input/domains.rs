//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private domain reconstruction preserves original declarations and legacy metadata absence.

use super::{expressions::payload, metadata::Identities, Fields};
use crate::{
    ast::{ConstraintCatalogIdentity, CreateDomain, DomainCheck, DomainNotNull},
    catalog::{domain::StoredDomain, type_definitions::TypeDefinitions},
    SQLError,
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken,
};

impl Fields<'_> {
    async fn constraint_identity(
        &mut self,
        identities: &mut Identities,
    ) -> Result<Option<ConstraintCatalogIdentity>, SQLError> {
        let Some(identity) = self.optional_catalog_identity().await? else {
            return Ok(None);
        };
        identities.object(identity.object_id)?;
        identities.oid(identity.oid as u32)?;
        Ok(Some(identity))
    }

    async fn domain_definition(
        &mut self,
        memory: &mut MemoryReservation,
        identities: &mut Identities,
    ) -> Result<CreateDomain, SQLError> {
        let name = payload(self.binary.text().await?, memory);
        let collation = payload(self.optional_text().await?, memory);
        let collation_oid = self.optional_number().await?;
        let has_default = self.flag().await?;
        let has_not_null = self.flag().await?;
        let count = self.binary.count().await?;
        let base = payload(self.column_type().await?, memory);
        let not_null = if has_not_null {
            Some(DomainNotNull {
                name: payload(self.optional_text().await?, memory),
                catalog_identity: self.constraint_identity(identities).await?,
            })
        } else {
            None
        };
        let default = if has_default {
            Some(payload(self.expression().await?, memory))
        } else {
            None
        };
        let mut checks = BudgetedVec::new(self.binary.memory);
        checks.reserve(count)?;
        for _ in 0..count {
            self.binary.checkpoint().await?;
            let name = payload(self.optional_text().await?, memory);
            let catalog_identity = self.constraint_identity(identities).await?;
            let validated = self.flag().await?;
            let expression = payload(self.expression().await?, memory);
            checks.push(DomainCheck {
                name,
                catalog_identity,
                expression,
                validated,
            })?;
        }
        let (checks, check_memory) = checks.into_parts();
        memory.absorb(check_memory);
        Ok(CreateDomain {
            name,
            base,
            collation,
            collation_oid,
            default,
            not_null,
            checks,
        })
    }

    async fn domain(
        &mut self,
        key: &str,
        identities: &mut Identities,
        constraints: &mut Identities,
    ) -> Result<Budgeted<StoredDomain>, SQLError> {
        let object_id = self.object_id().await?;
        identities.object(object_id)?;
        let oid = self.user_oid().await?;
        identities.oid(oid)?;
        let array_oid = self.optional_oid().await?;
        if let Some(oid) = array_oid {
            identities.oid(oid)?;
        }
        let mut memory = self.binary.memory.empty_reservation();
        let identity = payload(self.type_identity(key).await?, &mut memory);
        let owner = self.identity().await?;
        let array_name = payload(self.optional_text().await?, &mut memory);
        let usage_acl = payload(self.optional_object_acl().await?, &mut memory);
        let definition = self.domain_definition(&mut memory, constraints).await?;
        Ok(Budgeted::new(
            StoredDomain {
                object_id,
                oid,
                array_oid,
                identity,
                owner,
                definition,
                array_name,
                usage_acl,
            },
            memory,
        ))
    }
}

pub(in super::super) fn domains(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<TypeDefinitions<StoredDomain>, SQLError> {
    super::continuation::read_registry(reader, memory, cancellation)
}

impl TypeDefinitions<StoredDomain> {
    /// Retain original declarations, expressions and constraint identity claims across entry, byte and poll windows; later edits use the independent destination allowance.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry::<Self>(memory, mutations, cancellation)
    }
}

impl super::continuation::CatalogEntries for TypeDefinitions<StoredDomain> {
    type Scratch = (Identities, Identities);
    const MAGIC: [u8; 8] = *b"UQADOM01";
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (
            Self::with_budget(memory),
            (Identities::new(memory), Identities::new(memory)),
        )
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        scratch: &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let (identities, constraints) = scratch;
        let key = input.ordered_name(result.last_durable_key()).await?;
        let value = input.domain(&key, identities, constraints).await?;
        result.insert_image_entry(key, value)
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}
