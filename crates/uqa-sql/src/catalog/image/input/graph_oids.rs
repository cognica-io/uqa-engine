//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original graph catalog OIDs are restored without allocation or name-derived regeneration.

use super::{invalid, metadata::Identities, Fields};
use crate::{
    catalog::{
        graph_oids::{
            EndpointIndexOids, GraphCatalogOids, GraphOidRegistry, IndexConstraintOids,
            LabelCatalogOids,
        },
        relation_oids::RelationCatalogOids,
    },
    SQLError,
};
use std::{io::Read, ops::Bound};
use uqa_core::{
    memory::{Budgeted, MemoryBudget, OwnedMap},
    CancellationToken,
};

impl Fields<'_> {
    async fn not_null_oids(&mut self) -> Result<Budgeted<OwnedMap<String, u32>>, SQLError> {
        let count = self.binary.count().await?;
        let mut reservation = self.binary.memory.empty_reservation();
        let mut values = OwnedMap::<String, u32>::new();
        for _ in 0..count {
            self.binary.checkpoint().await?;
            let name = self.binary.text().await?;
            if name.is_empty() || values.first_from(Bound::Included(&*name)).is_some() {
                return Err(invalid("graph constraint name order"));
            }
            let oid = self.catalog_oid().await?;
            reservation.grow(OwnedMap::<String, u32>::entry_bytes())?;
            let (name, name_memory) = name.into_parts();
            reservation.absorb(name_memory);
            values.insert(name, oid);
        }
        Ok(Budgeted::new(values, reservation))
    }

    async fn graph_label(&mut self) -> Result<Budgeted<LabelCatalogOids>, SQLError> {
        let sequence = self.catalog_oid().await?;
        let relation = RelationCatalogOids {
            relation: self.catalog_oid().await?,
            row_type: self.optional_oid().await?,
            array_type: self.optional_oid().await?,
            rule: self.optional_oid().await?,
        };
        let id_default = self.catalog_oid().await?;
        let properties_default = self.catalog_oid().await?;
        let toast_table = self.catalog_oid().await?;
        let toast_index = self.catalog_oid().await?;
        let primary_key = if self.flag().await? {
            Some(IndexConstraintOids {
                index: self.catalog_oid().await?,
                constraint: self.catalog_oid().await?,
            })
        } else {
            None
        };
        let endpoint_indexes = if self.flag().await? {
            Some(EndpointIndexOids {
                start_id: self.catalog_oid().await?,
                end_id: self.catalog_oid().await?,
            })
        } else {
            None
        };
        let trigger = self.catalog_oid().await?;
        let (not_null, reservation) = self.not_null_oids().await?.into_parts();
        Ok(Budgeted::new(
            LabelCatalogOids {
                sequence,
                relation,
                id_default,
                properties_default,
                not_null,
                toast_table,
                toast_index,
                primary_key,
                endpoint_indexes,
                trigger,
            },
            reservation,
        ))
    }

    async fn graph_oids(
        &mut self,
        identities: &mut Identities,
    ) -> Result<Budgeted<GraphCatalogOids>, SQLError> {
        let namespace = self.catalog_oid().await?;
        let label_sequence = self.catalog_oid().await?;
        identities.oid(namespace)?;
        identities.oid(label_sequence)?;
        let count = self.binary.count().await?;
        let mut reservation = self.binary.memory.empty_reservation();
        let mut labels = OwnedMap::<u32, LabelCatalogOids>::new();
        for _ in 0..count {
            self.binary.checkpoint().await?;
            let key = u32::from_le_bytes(self.binary.array().await?);
            if key == 0 || labels.first_from(Bound::Included(&key)).is_some() {
                return Err(invalid("graph label key order"));
            }
            let label = self.graph_label().await?;
            for oid in label.claimed() {
                self.binary.checkpoint().await?;
                identities.oid(oid)?;
            }
            reservation.grow(OwnedMap::<u32, LabelCatalogOids>::entry_bytes())?;
            let (label, label_memory) = label.into_parts();
            reservation.absorb(label_memory);
            labels.insert(key, label);
        }
        Ok(Budgeted::new(
            GraphCatalogOids {
                namespace,
                label_sequence,
                labels,
            },
            reservation,
        ))
    }
}

pub(in super::super) fn graph_oids(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<GraphOidRegistry, SQLError> {
    super::continuation::read_registry(reader, memory, cancellation)
}

impl GraphOidRegistry {
    /// Retain native fields and claims across independent entry, byte and poll windows.
    pub fn image_input(
        memory: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry::<Self>(memory, mutations, cancellation)
    }
}

impl super::continuation::CatalogEntries for GraphOidRegistry {
    type Scratch = Identities;
    const MAGIC: [u8; 8] = super::super::GRAPH_OIDS;
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (Self::with_budget(memory), Identities::new(memory))
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        identities: &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let key = input.ordered_name(result.last_durable_key()).await?;
        let value = input.graph_oids(identities).await?;
        result.insert_image_entry(key, value)?;

        Ok(())
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}
