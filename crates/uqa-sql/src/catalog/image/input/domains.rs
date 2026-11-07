//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private domain reconstruction preserves original declarations and legacy metadata absence.

use super::{expressions::payload, metadata::Identities, Input};
use crate::{
    ast::{ConstraintCatalogIdentity, CreateDomain, DomainCheck, DomainNotNull},
    catalog::{domain::StoredDomain, type_definitions::TypeDefinitions},
    SQLError,
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, MemoryBudget, MemoryReservation},
    CancellationToken,
};

impl Input<'_> {
    fn constraint_identity(
        &mut self,
        identities: &mut Identities,
    ) -> Result<Option<ConstraintCatalogIdentity>, SQLError> {
        if !self.flag()? {
            return Ok(None);
        }
        let object_id = self.object_id()?;
        let oid = self.oid()?;
        identities.object(object_id)?;
        identities.oid(oid as u32)?;
        Ok(Some(ConstraintCatalogIdentity { object_id, oid }))
    }

    fn domain_definition(
        &mut self,
        memory: &mut MemoryReservation,
        identities: &mut Identities,
    ) -> Result<CreateDomain, SQLError> {
        let name = payload(self.binary.text()?, memory);
        let collation = payload(self.optional_text()?, memory);
        let collation_oid = self.optional_number()?;
        let has_default = self.flag()?;
        let has_not_null = self.flag()?;
        let count = self.binary.count()?;
        let base = payload(self.column_type()?, memory);
        let not_null = if has_not_null {
            Some(DomainNotNull {
                name: payload(self.optional_text()?, memory),
                catalog_identity: self.constraint_identity(identities)?,
            })
        } else {
            None
        };
        let default = if has_default {
            Some(payload(self.expression()?, memory))
        } else {
            None
        };
        let checks = self.items(count, memory, |input, memory| {
            let name = payload(input.optional_text()?, memory);
            let catalog_identity = input.constraint_identity(identities)?;
            let validated = input.flag()?;
            let expression = payload(input.expression()?, memory);
            Ok(DomainCheck {
                name,
                catalog_identity,
                expression,
                validated,
            })
        })?;
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

    fn domain(
        &mut self,
        key: &str,
        identities: &mut Identities,
        constraints: &mut Identities,
    ) -> Result<Budgeted<StoredDomain>, SQLError> {
        let object_id = self.object_id()?;
        identities.object(object_id)?;
        let oid = self.user_oid()?;
        identities.oid(oid)?;
        let array_oid = self.optional_oid()?;
        if let Some(oid) = array_oid {
            identities.oid(oid)?;
        }
        let mut memory = self.binary.memory.empty_reservation();
        let identity = payload(self.type_identity(key)?, &mut memory);
        let owner = self.identity()?;
        let array_name = payload(self.optional_text()?, &mut memory);
        let usage_acl = payload(self.optional_object_acl()?, &mut memory);
        let definition = self.domain_definition(&mut memory, constraints)?;
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
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(*b"UQADOM01")?;
    let mut result = TypeDefinitions::with_budget(memory);
    let mut identities = Identities::new(memory);
    let mut constraints = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let key = input.ordered_name(result.last_durable_key())?;
        let value = input.domain(&key, &mut identities, &mut constraints)?;
        result.insert_image_entry(key, value)?;
    }
    input.binary.end()?;
    Ok(result)
}
