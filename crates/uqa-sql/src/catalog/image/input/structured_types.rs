//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete composite and range images reconstruct private native catalogs without type binding or support-routine invocation.

use super::{invalid, metadata::Identities, Fields};
use crate::{
    catalog::{
        array_type_names::valid_type_name,
        composite_type::{StoredComposite, StoredCompositeAttribute},
        range_type::{StoredMultirange, StoredRange},
        type_definitions::TypeDefinitions,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken,
};

impl Fields<'_> {
    async fn array_name(&mut self, memory: &mut MemoryReservation) -> Result<String, SQLError> {
        let (name, reservation) = self.binary.text().await?.into_parts();
        memory.absorb(reservation);
        if !valid_type_name(&name) {
            return Err(invalid("generated array name"));
        }
        Ok(name)
    }
    async fn composite_attribute(
        &mut self,
    ) -> Result<Budgeted<StoredCompositeAttribute>, SQLError> {
        let name = self.binary.text().await?;
        let collation = self.optional_text().await?;
        let collation_oid = self.optional_number().await?;
        let number = i16::from_le_bytes(self.binary.array().await?);
        let dropped = self.flag().await?;
        if number < 1 || name.is_empty() || name.contains('\0') {
            return Err(invalid("composite attribute identity"));
        }
        let ty = self.column_type().await?;
        let mut memory = self.binary.memory.empty_reservation();
        let (name, name_memory) = name.into_parts();
        memory.absorb(name_memory);
        let (collation, collation_memory) = collation.into_parts();
        memory.absorb(collation_memory);
        let (ty, type_memory) = ty.into_parts();
        memory.absorb(type_memory);
        Ok(Budgeted::new(
            StoredCompositeAttribute {
                name,
                ty,
                collation,
                collation_oid,
                number,
                dropped,
            },
            memory,
        ))
    }
    async fn composite_attributes(
        &mut self,
        count: usize,
    ) -> Result<Budgeted<Vec<StoredCompositeAttribute>>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let mut attributes = BudgetedVec::<StoredCompositeAttribute>::new(self.binary.memory);
        attributes.reserve(count)?;
        for _ in 0..count {
            self.binary.checkpoint().await?;
            let attribute = self.composite_attribute().await?;
            if attributes
                .last()
                .is_some_and(|previous| previous.number >= attribute.number)
            {
                return Err(invalid("composite attribute order"));
            }
            let (attribute, reservation) = attribute.into_parts();
            memory.absorb(reservation);
            attributes.push(attribute)?;
        }
        let mut names = BudgetedMap::new(self.binary.memory);
        for attribute in attributes.iter().filter(|attribute| !attribute.dropped) {
            self.binary.checkpoint().await?;
            if names.insert(attribute.name.as_str(), ())?.is_some() {
                return Err(invalid("duplicate composite attribute"));
            }
        }
        drop(names);
        let (attributes, reservation) = attributes.into_parts();
        memory.absorb(reservation);
        Ok(Budgeted::new(attributes, memory))
    }
    async fn composite(
        &mut self,
        key: &str,
        identities: &mut Identities,
    ) -> Result<Budgeted<StoredComposite>, SQLError> {
        let object_id = self.object_id().await?;
        identities.object(object_id)?;
        let oid = self.user_oid().await?;
        identities.oid(oid)?;
        let relation_oid = self.user_oid().await?;
        identities.oid(relation_oid)?;
        let array_oid = self.user_oid().await?;
        identities.oid(array_oid)?;
        let mut memory = self.binary.memory.empty_reservation();
        let array_name = self.array_name(&mut memory).await?;
        let (identity, reservation) = self.type_identity(key).await?.into_parts();
        memory.absorb(reservation);
        let owner = self.identity().await?;
        let count = self.binary.count().await?;
        let (usage_acl, reservation) = self.optional_object_acl().await?.into_parts();
        memory.absorb(reservation);
        let (attributes, reservation) = self.composite_attributes(count).await?.into_parts();
        memory.absorb(reservation);
        Ok(Budgeted::new(
            StoredComposite {
                object_id,
                oid,
                relation_oid,
                array_oid,
                array_name,
                identity,
                owner,
                attributes,
                usage_acl,
            },
            memory,
        ))
    }
    async fn multirange(
        &mut self,
        identities: &mut Identities,
    ) -> Result<Budgeted<StoredMultirange>, SQLError> {
        let object_id = self.object_id().await?;
        identities.object(object_id)?;
        let oid = self.user_oid().await?;
        identities.oid(oid)?;
        let array_oid = self.user_oid().await?;
        identities.oid(array_oid)?;
        let mut memory = self.binary.memory.empty_reservation();
        let array_name = self.array_name(&mut memory).await?;
        let (identity, reservation) = self.durable_type_identity().await?.into_parts();
        memory.absorb(reservation);
        Ok(Budgeted::new(
            StoredMultirange {
                object_id,
                oid,
                array_oid,
                array_name,
                identity,
            },
            memory,
        ))
    }
    async fn range(
        &mut self,
        key: &str,
        identities: &mut Identities,
    ) -> Result<Budgeted<StoredRange>, SQLError> {
        let object_id = self.object_id().await?;
        identities.object(object_id)?;
        let oid = self.user_oid().await?;
        identities.oid(oid)?;
        let array_oid = self.user_oid().await?;
        identities.oid(array_oid)?;
        let mut memory = self.binary.memory.empty_reservation();
        let array_name = self.array_name(&mut memory).await?;
        let (identity, reservation) = self.type_identity(key).await?.into_parts();
        memory.absorb(reservation);
        let owner = self.identity().await?;
        let subtype_operator_class = self.catalog_oid().await?;
        let collation_oid = u32::from_le_bytes(self.binary.array().await?);
        let canonical_oid = self.optional_oid().await?;
        let subtype_difference_oid = self.optional_oid().await?;
        let (usage_acl, reservation) = self.optional_object_acl().await?.into_parts();
        memory.absorb(reservation);
        let (subtype, reservation) = self.column_type().await?.into_parts();
        memory.absorb(reservation);
        if !crate::catalog::range_type::resolved_subtype(&subtype) {
            return Err(invalid("unresolved range subtype"));
        }
        let (multirange, reservation) = self.multirange(identities).await?.into_parts();
        memory.absorb(reservation);
        Ok(Budgeted::new(
            StoredRange {
                object_id,
                oid,
                array_oid,
                array_name,
                identity,
                owner,
                subtype,
                subtype_operator_class,
                collation_oid,
                canonical_oid,
                subtype_difference_oid,
                multirange,
                usage_acl,
            },
            memory,
        ))
    }
}

pub(in super::super) fn composites(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<TypeDefinitions<StoredComposite>, SQLError> {
    super::continuation::read_registry(reader, memory, cancellation)
}

impl TypeDefinitions<StoredComposite> {
    /// Retain native entries and identity claims between calls; future mutations use the destination allowance.
    pub fn image_input(
        input: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry::<Self>(input, mutations, cancellation)
    }
}

pub(in super::super) fn ranges(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<TypeDefinitions<StoredRange>, SQLError> {
    super::continuation::read_registry(reader, memory, cancellation)
}

impl TypeDefinitions<StoredRange> {
    /// Retain native entries, identities and range/array name claims between calls; future mutations use the destination allowance.
    pub fn image_input(
        input: &MemoryBudget,
        mutations: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::CatalogImageInput<Self>, SQLError> {
        super::continuation::registry::<Self>(input, mutations, cancellation)
    }
}

impl super::continuation::CatalogEntries for TypeDefinitions<StoredComposite> {
    type Scratch = Identities;
    const MAGIC: [u8; 8] = super::super::COMPOSITES;
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (Self::with_budget(memory), Identities::new(memory))
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        scratch: &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let identities = scratch;
        let key = input.ordered_name(result.last_durable_key()).await?;
        let value = input.composite(&key, identities).await?;
        result.insert_image_entry(key, value)?;
        Ok(())
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}

impl super::continuation::CatalogEntries for TypeDefinitions<StoredRange> {
    type Scratch = (Identities, BudgetedMap<(String, String), MemoryReservation>);
    const MAGIC: [u8; 8] = super::super::RANGES;
    fn create(memory: &MemoryBudget) -> (Self, Self::Scratch) {
        (
            Self::with_budget(memory),
            (Identities::new(memory), BudgetedMap::new(memory)),
        )
    }
    async fn read_entry(
        input: &mut Fields<'_>,
        result: &mut Self,
        scratch: &mut Self::Scratch,
    ) -> Result<(), SQLError> {
        let (identities, names) = scratch;
        let key = input.ordered_name(result.last_durable_key()).await?;
        let value = input.range(&key, identities).await?;
        for (identity, array) in [
            (&value.identity, &value.array_name),
            (&value.multirange.identity, &value.multirange.array_name),
        ] {
            for name in [&identity.name, array] {
                let bytes = identity
                    .schema
                    .len()
                    .checked_add(name.len())
                    .ok_or_else(|| invalid("range name length"))?;
                let reservation = input.binary.memory.reserve(bytes)?;
                let name = (identity.schema.clone(), name.clone());
                if names.insert(name, reservation)?.is_some() {
                    return Err(invalid("duplicate range type name"));
                }
            }
        }
        result.insert_image_entry(key, value)?;
        Ok(())
    }
    fn finish(self, mutations: &MemoryBudget) -> Self {
        self.into_mutation_budget(mutations)
    }
}
