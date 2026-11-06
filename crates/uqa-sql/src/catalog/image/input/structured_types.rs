//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete composite and range images reconstruct private native catalogs without type binding or support-routine invocation.

use super::{invalid, metadata::Identities, Input};
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

impl Input<'_> {
    fn array_name(&mut self, memory: &mut MemoryReservation) -> Result<String, SQLError> {
        let (name, reservation) = self.binary.text()?.into_parts();
        memory.absorb(reservation);
        if !valid_type_name(&name) {
            return Err(invalid("generated array name"));
        }
        Ok(name)
    }
    fn composite_attribute(&mut self) -> Result<Budgeted<StoredCompositeAttribute>, SQLError> {
        let (name, mut memory) = self.binary.text()?.into_parts();
        let (collation, collation_memory) = self.optional_text()?.into_parts();
        memory.absorb(collation_memory);
        let collation_oid = self.optional_number()?;
        let number = i16::from_le_bytes(self.binary.array()?);
        let dropped = self.flag()?;
        if number < 1 || name.is_empty() || name.contains('\0') {
            return Err(invalid("composite attribute identity"));
        }
        let (ty, type_memory) = self.column_type()?.into_parts();
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
    fn composite_attributes(
        &mut self,
        count: usize,
    ) -> Result<Budgeted<Vec<StoredCompositeAttribute>>, SQLError> {
        let mut memory = self.binary.memory.empty_reservation();
        let mut attributes = BudgetedVec::<StoredCompositeAttribute>::new(self.binary.memory);
        attributes.reserve(count)?;
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let attribute = self.composite_attribute()?;
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
            self.binary.cancellation.check()?;
            if names.insert(attribute.name.as_str(), ())?.is_some() {
                return Err(invalid("duplicate composite attribute"));
            }
        }
        drop(names);
        let (attributes, reservation) = attributes.into_parts();
        memory.absorb(reservation);
        Ok(Budgeted::new(attributes, memory))
    }
    fn composite(
        &mut self,
        key: &str,
        identities: &mut Identities,
    ) -> Result<Budgeted<StoredComposite>, SQLError> {
        let object_id = self.object_id()?;
        identities.object(object_id)?;
        let oid = self.user_oid()?;
        identities.oid(oid)?;
        let relation_oid = self.user_oid()?;
        identities.oid(relation_oid)?;
        let array_oid = self.user_oid()?;
        identities.oid(array_oid)?;
        let mut memory = self.binary.memory.empty_reservation();
        let array_name = self.array_name(&mut memory)?;
        let (identity, reservation) = self.type_identity(key)?.into_parts();
        memory.absorb(reservation);
        let owner = self.identity()?;
        let count = self.binary.count()?;
        let (usage_acl, reservation) = self.optional_object_acl()?.into_parts();
        memory.absorb(reservation);
        let (attributes, reservation) = self.composite_attributes(count)?.into_parts();
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
    fn multirange(
        &mut self,
        identities: &mut Identities,
    ) -> Result<Budgeted<StoredMultirange>, SQLError> {
        let object_id = self.object_id()?;
        identities.object(object_id)?;
        let oid = self.user_oid()?;
        identities.oid(oid)?;
        let array_oid = self.user_oid()?;
        identities.oid(array_oid)?;
        let mut memory = self.binary.memory.empty_reservation();
        let array_name = self.array_name(&mut memory)?;
        let (identity, reservation) = self.durable_type_identity()?.into_parts();
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
    fn range(
        &mut self,
        key: &str,
        identities: &mut Identities,
    ) -> Result<Budgeted<StoredRange>, SQLError> {
        let object_id = self.object_id()?;
        identities.object(object_id)?;
        let oid = self.user_oid()?;
        identities.oid(oid)?;
        let array_oid = self.user_oid()?;
        identities.oid(array_oid)?;
        let mut memory = self.binary.memory.empty_reservation();
        let array_name = self.array_name(&mut memory)?;
        let (identity, reservation) = self.type_identity(key)?.into_parts();
        memory.absorb(reservation);
        let owner = self.identity()?;
        let subtype_operator_class = self.catalog_oid()?;
        let collation_oid = u32::from_le_bytes(self.binary.array()?);
        let canonical_oid = self.optional_oid()?;
        let subtype_difference_oid = self.optional_oid()?;
        let (usage_acl, reservation) = self.optional_object_acl()?.into_parts();
        memory.absorb(reservation);
        let (subtype, reservation) = self.column_type()?.into_parts();
        memory.absorb(reservation);
        if !crate::catalog::range_type::resolved_subtype(&subtype) {
            return Err(invalid("unresolved range subtype"));
        }
        let (multirange, reservation) = self.multirange(identities)?.into_parts();
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
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::COMPOSITES)?;
    let mut result = TypeDefinitions::with_budget(memory);
    let mut identities = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let key = input.ordered_name(result.last_durable_key())?;
        let value = input.composite(&key, &mut identities)?;
        result.insert_image_entry(key, value)?;
    }
    input.binary.end()?;
    Ok(result)
}

pub(in super::super) fn ranges(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<TypeDefinitions<StoredRange>, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::RANGES)?;
    let mut result = TypeDefinitions::with_budget(memory);
    let mut identities = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let key = input.ordered_name(result.last_durable_key())?;
        let value = input.range(&key, &mut identities)?;
        result.insert_image_entry(key, value)?;
    }
    input.binary.end()?;
    let mut names = BudgetedMap::new(memory);
    for value in result.values() {
        cancellation.check()?;
        for (identity, array) in [
            (&value.identity, &value.array_name),
            (&value.multirange.identity, &value.multirange.array_name),
        ] {
            for name in [&identity.name, array] {
                if names
                    .insert((identity.schema.as_str(), name.as_str()), ())?
                    .is_some()
                {
                    return Err(invalid("duplicate range type name"));
                }
            }
        }
    }
    drop(names);
    Ok(result)
}
