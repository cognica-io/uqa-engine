//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table, domain and constraint-trigger rows share the constraint catalog namespace.

use super::{
    invalid, retire_entries, BudgetedMap, CatalogWork, MemoryBudget, NativeConstraintTable,
    ReferenceWork,
};
use crate::{
    ast::CreateDomain,
    catalog::events::StoredTrigger,
    schema::{constraint_metadata::identity::claims::row_slots, domains::constraints},
    SQLError,
};

pub(super) struct Identities {
    objects: BudgetedMap<[u8; 16], ()>,
    oids: BudgetedMap<i64, ()>,
}

impl Identities {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            objects: BudgetedMap::new(memory),
            oids: BudgetedMap::new(memory),
        }
    }

    fn insert(&mut self, object_id: Option<[u8; 16]>, oid: Option<i64>) -> Result<(), SQLError> {
        if object_id.is_some_and(|id| id == [0; 16])
            || oid.is_some_and(|oid| !u32::try_from(oid).is_ok_and(|oid| oid != 0))
        {
            return Err(invalid("invalid constraint catalog identity"));
        }
        if let Some(object_id) = object_id {
            if self.objects.insert(object_id, ())?.is_some() {
                return Err(invalid("duplicate constraint catalog incarnation"));
            }
        }
        if let Some(oid) = oid {
            if self.oids.insert(oid, ())?.is_some() {
                return Err(invalid("duplicate constraint catalog OID"));
            }
        }
        Ok(())
    }

    pub(super) async fn table(
        &mut self,
        table: NativeConstraintTable<'_>,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        for slot in row_slots(table.columns, table.checks, table.keys, table.foreign_keys) {
            input.checkpoint().await?;
            if let Some(identity) = slot {
                self.insert(Some(identity.object_id), Some(identity.oid))?;
            }
        }
        Ok(())
    }

    pub(super) async fn domain(
        &mut self,
        domain: &CreateDomain,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        input.checkpoint().await?;
        for slot in constraints::identity_slots(domain) {
            input.checkpoint().await?;
            if let Some(identity) = slot {
                self.insert(Some(identity.object_id), Some(identity.oid))?;
            }
        }
        Ok(())
    }

    pub(super) async fn trigger(
        &mut self,
        trigger: &StoredTrigger,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        input.checkpoint().await?;
        if trigger.definition.constraint {
            self.insert(trigger.object_id, trigger.constraint_catalog_oid)?;
        }
        Ok(())
    }

    pub(super) async fn retire(
        self,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<(), SQLError> {
        retire_entries(self.objects.into_entries(), input.control, input.work).await?;
        retire_entries(self.oids.into_entries(), input.control, input.work).await
    }
}
