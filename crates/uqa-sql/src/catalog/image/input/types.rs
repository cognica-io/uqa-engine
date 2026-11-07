//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native enum and shell reconstruction retains declared identities, label keys and float4 positions.

use super::{invalid, metadata::Identities, Input};
use crate::{
    catalog::{
        array_type_names::valid_type_name,
        enum_type::{StoredEnum, StoredEnumLabel, MAX_ENUM_LABEL_BYTES},
        shell_type::StoredShellType,
        type_definitions::TypeDefinitions,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryBudget},
    CancellationToken, EnumLabelKey, MAX_ENUM_LABEL_KEY_BYTES,
};

impl Input<'_> {
    fn enum_label(
        &mut self,
        identities: &mut Identities,
    ) -> Result<Budgeted<StoredEnumLabel>, SQLError> {
        let oid = self.user_oid()?;
        identities.oid(oid)?;
        let (key, mut reservation) = self
            .binary
            .bytes_with_limit(MAX_ENUM_LABEL_KEY_BYTES)?
            .into_parts();
        let source_bytes = key.capacity();
        // Compact key construction can allocate before retiring its original vector.
        reservation.grow(key.len())?;
        let key = EnumLabelKey::from_bytes(key).map_err(|_| invalid("enum order key"))?;
        drop(reservation.split(source_bytes));
        let (label, label_memory) = self
            .binary
            .bytes_with_limit(MAX_ENUM_LABEL_BYTES)?
            .into_parts();
        reservation.absorb(label_memory);
        let label = String::from_utf8(label).map_err(|_| invalid("enum label UTF-8"))?;
        let sort_order = f32::from_bits(u32::from_le_bytes(self.binary.array()?));
        if !sort_order.is_finite() {
            return Err(invalid("enum sort position"));
        }
        Ok(Budgeted::new(
            StoredEnumLabel {
                oid,
                key,
                label,
                sort_order,
            },
            reservation,
        ))
    }

    fn enum_labels(
        &mut self,
        count: usize,
        identities: &mut Identities,
    ) -> Result<Budgeted<Vec<StoredEnumLabel>>, SQLError> {
        let mut reservation = self.binary.memory.empty_reservation();
        let mut labels = BudgetedVec::<StoredEnumLabel>::new(self.binary.memory);
        labels.reserve(count)?;
        for _ in 0..count {
            self.binary.cancellation.check()?;
            let label = self.enum_label(identities)?;
            if labels.last().is_some_and(|previous| {
                previous.key >= label.key || previous.sort_order >= label.sort_order
            }) {
                return Err(invalid("enum label order"));
            }
            let (label, label_memory) = label.into_parts();
            reservation.absorb(label_memory);
            labels.push(label)?;
        }
        // Borrow the completed payloads while checking names; no second copy of the label corpus.
        let mut names = BudgetedMap::new(self.binary.memory);
        for label in labels.iter() {
            self.binary.cancellation.check()?;
            if names.insert(label.label.as_str(), ())?.is_some() {
                return Err(invalid("duplicate enum label"));
            }
        }
        drop(names);
        let (labels, label_memory) = labels.into_parts();
        reservation.absorb(label_memory);
        Ok(Budgeted::new(labels, reservation))
    }

    fn enum_type(
        &mut self,
        key: &str,
        identities: &mut Identities,
    ) -> Result<Budgeted<StoredEnum>, SQLError> {
        let object_id = self.object_id()?;
        identities.object(object_id)?;
        let oid = self.user_oid()?;
        let array_oid = self.user_oid()?;
        identities.oid(oid)?;
        identities.oid(array_oid)?;
        let mut reservation = self.binary.memory.empty_reservation();
        let (array_name, array_memory) = self.binary.text()?.into_parts();
        reservation.absorb(array_memory);
        if !valid_type_name(&array_name) {
            return Err(invalid("enum array name"));
        }
        let (identity, identity_memory) = self.type_identity(key)?.into_parts();
        reservation.absorb(identity_memory);
        let owner = self.identity()?;
        let count = self.binary.count()?;
        let (usage_acl, acl_memory) = self.optional_object_acl()?.into_parts();
        reservation.absorb(acl_memory);
        let (labels, labels_memory) = self.enum_labels(count, identities)?.into_parts();
        reservation.absorb(labels_memory);
        Ok(Budgeted::new(
            StoredEnum {
                object_id,
                oid,
                array_oid,
                array_name,
                identity,
                owner,
                labels,
                usage_acl,
            },
            reservation,
        ))
    }

    fn shell(
        &mut self,
        key: &str,
        identities: &mut Identities,
    ) -> Result<Budgeted<StoredShellType>, SQLError> {
        let object_id = self.object_id()?;
        identities.object(object_id)?;
        let oid = self.user_oid()?;
        identities.oid(oid)?;
        let mut reservation = self.binary.memory.empty_reservation();
        let (identity, identity_memory) = self.type_identity(key)?.into_parts();
        reservation.absorb(identity_memory);
        let owner = self.identity()?;
        let (usage_acl, acl_memory) = self.optional_object_acl()?.into_parts();
        reservation.absorb(acl_memory);
        Ok(Budgeted::new(
            StoredShellType {
                object_id,
                oid,
                identity,
                owner,
                usage_acl,
            },
            reservation,
        ))
    }
}

pub(in super::super) fn enums(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<TypeDefinitions<StoredEnum>, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::ENUMS)?;
    let mut result = TypeDefinitions::with_budget(memory);
    let mut identities = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let key = input.ordered_name(result.last_durable_key())?;
        let value = input.enum_type(&key, &mut identities)?;
        result.insert_image_entry(key, value)?;
    }
    input.binary.end()?;
    Ok(result)
}

pub(in super::super) fn shells(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<TypeDefinitions<StoredShellType>, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::SHELLS)?;
    let mut result = TypeDefinitions::with_budget(memory);
    let mut identities = Identities::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let key = input.ordered_name(result.last_durable_key())?;
        let value = input.shell(&key, &mut identities)?;
        result.insert_image_entry(key, value)?;
    }
    input.binary.end()?;
    Ok(result)
}
