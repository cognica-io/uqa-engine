//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native routine and system relation authority is decoded before any catalog publication.

use super::{invalid, Fields};
use crate::{
    catalog::{
        security::{
            builtin_routines::{BuiltinRoutineSecurities, BuiltinRoutineSecurity},
            system_relations::{SystemAcl, SystemRelationSecurities, SystemRelationSecurity},
        },
        SystemRelation,
    },
    SQLError,
};
use std::{cmp::Ordering, io::Read, ops::Bound};
use uqa_core::{
    memory::{Budgeted, MemoryBudget, OwnedMap, ProductionControl},
    CancellationToken, RelationIdentity,
};

impl Fields<'_> {
    async fn revision(&mut self) -> Result<[u8; 16], SQLError> {
        let revision = self.binary.array().await?;
        if revision == [0; 16] {
            return Err(invalid("authority tuple revision"));
        }
        Ok(revision)
    }

    async fn system_acl(&mut self) -> Result<Budgeted<SystemAcl>, SQLError> {
        let revision = self.revision().await?;
        let (acl, reservation) = self.acl_entries().await?.into_parts();
        Ok(Budgeted::new(SystemAcl { revision, acl }, reservation))
    }

    async fn relation_key(
        &mut self,
        previous: Option<&RelationIdentity>,
    ) -> Result<(Budgeted<RelationIdentity>, SystemRelation), SQLError> {
        let schema = self.binary.text().await?;
        let name = self.binary.text().await?;
        let relation = SystemRelation::at(&schema, &name)
            .ok_or_else(|| invalid("system relation identity"))?;
        if let Some(previous) = previous {
            let mut ordering = self.compare_text(&previous.schema, &schema).await?;
            if ordering == Ordering::Equal {
                ordering = self.compare_text(&previous.name, &name).await?;
            }
            if ordering != Ordering::Less {
                return Err(invalid("system relation key order"));
            }
        }
        let (schema, mut reservation) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        reservation.absorb(name_memory);
        Ok((
            Budgeted::new(RelationIdentity { schema, name }, reservation),
            relation,
        ))
    }

    async fn relation_authority(
        &mut self,
        relation: SystemRelation,
    ) -> Result<Budgeted<SystemRelationSecurity>, SQLError> {
        let count = self.binary.count().await?;
        // Declare the aggregate lease first so partial native values retire before admission.
        let mut reservation = self.binary.memory.empty_reservation();
        let mut value = SystemRelationSecurity::default();
        if self.flag().await? {
            let (table, table_memory) = self.system_acl().await?.into_parts();
            reservation.absorb(table_memory);
            value.table = Some(table);
        }
        if count != 0 {
            let control = ProductionControl::new(
                self.binary.memory,
                self.binary.cancellation,
                self.binary.cancellation,
            );
            let columns = relation.column_names_with_control(&control)?;
            if count > columns.len() {
                return Err(invalid("system column count"));
            }
            for _ in 0..count {
                self.binary.checkpoint().await?;
                let name = self.binary.text().await?;
                if !columns.contains(&name)
                    || value.columns.first_from(Bound::Included(&*name)).is_some()
                {
                    return Err(invalid("system column identity or order"));
                }
                let acl = self.system_acl().await?;
                reservation.grow(OwnedMap::<String, SystemAcl>::entry_bytes())?;
                let (name, name_memory) = name.into_parts();
                let (acl, acl_memory) = acl.into_parts();
                reservation.absorb(name_memory);
                reservation.absorb(acl_memory);
                value.columns.insert(name, acl);
            }
        }
        Ok(Budgeted::new(value, reservation))
    }
}

pub(in super::super) fn builtin_routines(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<BuiltinRoutineSecurities, SQLError> {
    super::continuation::read_registry(reader, memory, cancellation)
}

impl BuiltinRoutineSecurities {
    /// Retain native fields and claims across independent entry, byte and poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry::<Self>(memory, mutations, cancellation)
    }
}

impl super::continuation::CatalogEntries for BuiltinRoutineSecurities {
    type Scratch = ();
    const MAGIC: [u8; 8] = super::super::BUILTIN_ROUTINES;
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (Self::with_budget(memory), ())
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        (): &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let oid = u32::from_le_bytes(input.binary.array().await?);
        if oid == 0
            || result
                .last_durable_key()
                .is_some_and(|previous| *previous >= oid)
        {
            return Err(invalid("builtin routine key order"));
        }
        let revision = input.revision().await?;
        let (execute_acl, reservation) = input.object_acl().await?.into_parts();
        result.insert_image_entry(
            Budgeted::new(oid, input.binary.memory.empty_reservation()),
            Budgeted::new(
                BuiltinRoutineSecurity {
                    revision,
                    execute_acl,
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
pub(in super::super) fn system_relations(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<SystemRelationSecurities, SQLError> {
    super::continuation::read_registry(reader, memory, cancellation)
}

impl SystemRelationSecurities {
    /// Retain native fields and claims across independent entry, byte and poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry::<Self>(memory, mutations, cancellation)
    }
}

impl super::continuation::CatalogEntries for SystemRelationSecurities {
    type Scratch = ();
    const MAGIC: [u8; 8] = super::super::SYSTEM_RELATIONS;
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (Self::with_budget(memory), ())
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        (): &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let (key, relation) = input.relation_key(result.last_durable_key()).await?;
        let security = input.relation_authority(relation).await?;
        result.insert_image_entry(key, security)?;

        Ok(())
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}
