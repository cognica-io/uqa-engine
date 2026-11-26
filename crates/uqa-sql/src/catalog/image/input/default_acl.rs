//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{continuation, invalid, Fields};
use crate::{
    ast::DefaultAclObject,
    catalog::security::default_acl::{DefaultAclKey, DefaultAclRow, DefaultAcls, DefaultRights},
    SQLError,
};
use std::io::Read;
use uqa_core::{
    catalog_role::BoundAclEntry,
    memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryBudget},
    CancellationToken,
};

pub(in super::super) fn default_acls(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<DefaultAcls, SQLError> {
    continuation::read_registry(reader, memory, cancellation)
}

impl DefaultAcls {
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        continuation::registry::<Self>(memory, mutations, cancellation)
    }
}

impl continuation::CatalogEntries for DefaultAcls {
    type Scratch = BudgetedMap<u32, ()>;
    const MAGIC: [u8; 8] = super::super::DEFAULT_ACLS;
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (Self::with_budget(memory), BudgetedMap::new(memory))
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        oids: &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let owner = input.identity().await?;
        let namespace = u32::from_le_bytes(input.binary.array().await?);
        let object = DefaultAclObject::from_code(input.binary.array::<1>().await?[0])
            .ok_or_else(|| invalid("default ACL object class"))?;
        let key = DefaultAclKey {
            owner,
            namespace,
            object,
        };
        if result.last_durable_key().is_some_and(|prior| *prior >= key) {
            return Err(invalid("default ACL key order"));
        }
        let oid = u32::from_le_bytes(input.binary.array().await?);
        if oids.insert(oid, ())?.is_some() {
            return Err(invalid("duplicate default ACL OID"));
        }
        let count = input.binary.count().await?;
        let mut acl = BudgetedVec::new(input.binary.memory);
        acl.reserve(count)?;
        for _ in 0..count {
            let role = if input.flag().await? {
                Some(input.identity().await?)
            } else {
                None
            };
            let privileges = DefaultRights(u16::from_le_bytes(input.binary.array().await?));
            let grant_options = DefaultRights(u16::from_le_bytes(input.binary.array().await?));
            let entry = BoundAclEntry {
                role,
                grantor: owner,
                privileges,
                grant_options,
            };
            key.validate_entry(&entry, acl.last())?;
            acl.push(entry)?;
        }
        let (acl, memory) = acl.into_parts();
        let row = DefaultAclRow {
            oid,
            temporary: false,
            acl,
        };
        row.validate_header(key)?;
        result.insert_image_entry(
            Budgeted::new(key, input.binary.memory.empty_reservation()),
            Budgeted::new(row, memory),
        )?;
        Ok(())
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}
