//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original graph catalog OIDs are restored without allocation or name-derived regeneration.

use super::{invalid, metadata::Identities, Input};
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

impl Input<'_> {
    fn not_null_oids(&mut self) -> Result<Budgeted<OwnedMap<String, u32>>, SQLError> {
        let count = self.binary.count()?;
        let mut reservation = self.binary.memory.empty_reservation();
        let mut values = OwnedMap::<String, u32>::new();
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let name = self.binary.text()?;
            if name.is_empty() || values.first_from(Bound::Included(&*name)).is_some() {
                return Err(invalid("graph constraint name order"));
            }
            let oid = self.catalog_oid()?;
            reservation.grow(OwnedMap::<String, u32>::entry_bytes())?;
            let (name, name_memory) = name.into_parts();
            reservation.absorb(name_memory);
            values.insert(name, oid);
        }
        Ok(Budgeted::new(values, reservation))
    }

    fn graph_label(&mut self) -> Result<Budgeted<LabelCatalogOids>, SQLError> {
        let sequence = self.catalog_oid()?;
        let relation = RelationCatalogOids {
            relation: self.catalog_oid()?,
            row_type: self.optional_oid()?,
            array_type: self.optional_oid()?,
            rule: self.optional_oid()?,
        };
        let id_default = self.catalog_oid()?;
        let properties_default = self.catalog_oid()?;
        let toast_table = self.catalog_oid()?;
        let toast_index = self.catalog_oid()?;
        let primary_key = if self.flag()? {
            Some(IndexConstraintOids {
                index: self.catalog_oid()?,
                constraint: self.catalog_oid()?,
            })
        } else {
            None
        };
        let endpoint_indexes = if self.flag()? {
            Some(EndpointIndexOids {
                start_id: self.catalog_oid()?,
                end_id: self.catalog_oid()?,
            })
        } else {
            None
        };
        let trigger = self.catalog_oid()?;
        let (not_null, reservation) = self.not_null_oids()?.into_parts();
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

    fn graph_oids(
        &mut self,
        identities: &mut Identities,
    ) -> Result<Budgeted<GraphCatalogOids>, SQLError> {
        let namespace = self.catalog_oid()?;
        let label_sequence = self.catalog_oid()?;
        identities.oid(namespace)?;
        identities.oid(label_sequence)?;
        let count = self.binary.count()?;
        let mut reservation = self.binary.memory.empty_reservation();
        let mut labels = OwnedMap::<u32, LabelCatalogOids>::new();
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let key = u32::from_le_bytes(self.binary.array()?);
            if key == 0 || labels.first_from(Bound::Included(&key)).is_some() {
                return Err(invalid("graph label key order"));
            }
            let label = self.graph_label()?;
            for oid in label.claimed() {
                self.binary.cancellation.check()?;
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
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::GRAPH_OIDS)?;
    let mut result = GraphOidRegistry::with_budget(memory);
    let mut identities = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let key = input.ordered_name(result.last_durable_key())?;
        let value = input.graph_oids(&mut identities)?;
        result.insert_image_entry(key, value)?;
    }
    input.binary.end()?;
    Ok(result)
}
