//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role definitions preserve native keys, tuple revisions and explicit attribute bits.

use super::{invalid, validate_identity, Input};
use crate::{
    ast::RoleAttribute,
    catalog::roles::{RoleDefinition, RoleDefinitions, RoleIdentity},
    SQLError,
};
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    memory::{Budgeted, BudgetedMap, MemoryBudget, OwnedSet},
    CancellationToken,
};

pub(in super::super) fn roles(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<RoleDefinitions, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::ROLES)?;
    let mut result = RoleDefinitions::with_budget(memory);
    let mut oids = BudgetedMap::new(memory);
    let mut identities = BudgetedMap::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let key = input.ordered_name(result.last_durable_key())?;
        let oid = input.oid()?;
        let object_id = input.binary.array()?;
        validate_identity(RoleIdentity { oid, object_id })?;
        let revision = u64::from_le_bytes(input.binary.array()?);
        if revision == 0 {
            return Err(invalid("role revision"));
        }
        let name = input.binary.text()?;
        if input.compare_text(&name, &key)? != Ordering::Equal {
            return Err(invalid("role definition key"));
        }
        let [bits] = input.binary.array()?;
        if bits & !127 != 0 {
            return Err(invalid("role attribute bits"));
        }
        let connection_limit = i32::from_le_bytes(input.binary.array()?);
        if connection_limit < -1 {
            return Err(invalid("connection limit"));
        }
        if oids.insert(oid, ())?.is_some() || identities.insert(object_id, ())?.is_some() {
            return Err(invalid("duplicate role identity"));
        }
        let attribute_memory = memory
            .reserve(bits.count_ones() as usize * OwnedSet::<RoleAttribute>::entry_bytes())?;
        let mut attributes = OwnedSet::new();
        for attribute in [
            RoleAttribute::Superuser,
            RoleAttribute::Inherit,
            RoleAttribute::CreateRole,
            RoleAttribute::CreateDb,
            RoleAttribute::Login,
            RoleAttribute::Replication,
            RoleAttribute::BypassRls,
        ] {
            if bits & super::super::fields::attribute_bit(attribute) != 0 {
                attributes.insert(attribute);
            }
        }
        let (name, mut reservation) = name.into_parts();
        reservation.absorb(attribute_memory);
        result.insert_image_entry(
            key,
            Budgeted::new(
                RoleDefinition {
                    oid,
                    object_id,
                    revision,
                    name,
                    attributes,
                    connection_limit,
                },
                reservation,
            ),
        )?;
    }
    input.binary.end()?;
    Ok(result)
}
