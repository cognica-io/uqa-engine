//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Membership decoding retains original bindings and verifies their explicit ordered keys.

use super::{invalid, Input};
use crate::{
    catalog::roles::{RoleMembership, RoleMembershipKey, RoleMemberships},
    SQLError,
};
use std::io::Read;
use uqa_core::{
    memory::{Budgeted, BudgetedMap, MemoryBudget},
    CancellationToken,
};

pub(in super::super) fn memberships(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<RoleMemberships, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::MEMBERSHIPS)?;
    let mut result = RoleMemberships::with_budget(memory);
    let mut oids = BudgetedMap::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let key = RoleMembershipKey {
            role: input.identity()?,
            member: input.identity()?,
            grantor: input.identity()?,
        };
        if result
            .last_durable_key()
            .is_some_and(|previous| *previous >= key)
        {
            return Err(invalid("membership order"));
        }
        let oid = input.oid()?;
        let role = input.binding()?;
        let member = input.binding()?;
        let grantor = input.binding()?;
        let [bits] = input.binary.array()?;
        if bits & !7 != 0 {
            return Err(invalid("membership option bits"));
        }
        if oids.insert(oid, ())?.is_some() {
            return Err(invalid("duplicate membership OID"));
        }
        let (role, mut reservation) = role.into_parts();
        let (member, member_memory) = member.into_parts();
        reservation.absorb(member_memory);
        let (grantor, grantor_memory) = grantor.into_parts();
        reservation.absorb(grantor_memory);
        let membership = Budgeted::new(
            RoleMembership {
                oid,
                role,
                member,
                grantor,
                admin_option: bits & 1 != 0,
                inherit_option: bits & 2 != 0,
                set_option: bits & 4 != 0,
            },
            reservation,
        );
        if membership.key() != key {
            return Err(invalid("membership key"));
        }
        result.insert_image_entry(Budgeted::new(key, memory.empty_reservation()), membership)?;
    }
    input.binary.end()?;
    Ok(result)
}
