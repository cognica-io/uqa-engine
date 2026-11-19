//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Membership validation preserves bound role incarnations and its original error order.

use super::{invalid, RoleRestorationError};
use crate::catalog::{
    named_definitions::NamedDefinitionIter,
    roles::{RoleDefinition, RoleDefinitions, RoleIdentity, RoleMembership, RoleMembershipKey},
};
use uqa_core::memory::{ProductionControl, ProductionMap};

pub(in crate::catalog::roles::restoration) struct MembershipChecks<'a> {
    identities: ProductionMap<'a, [u8; 16], i64>,
    roles: ProductionMap<'a, RoleIdentity, ()>,
    oids: ProductionMap<'a, i64, ()>,
    keys: ProductionMap<'a, RoleMembershipKey, ()>,
}

impl<'a> MembershipChecks<'a> {
    pub(in crate::catalog::roles::restoration) fn new(control: ProductionControl<'a>) -> Self {
        Self {
            identities: ProductionMap::new(control),
            roles: ProductionMap::new(control),
            oids: ProductionMap::new(control),
            keys: ProductionMap::new(control),
        }
    }

    pub(in crate::catalog::roles::restoration) fn role(
        &mut self,
        role: &RoleDefinition,
    ) -> Result<(), RoleRestorationError> {
        self.identities.insert_copy(role.object_id, role.oid)?;
        self.roles.insert_copy(role.identity(), ())?;
        Ok(())
    }

    pub(in crate::catalog::roles::restoration) fn membership(
        &mut self,
        membership: &RoleMembership,
    ) -> Result<(), RoleRestorationError> {
        for binding in [&membership.role, &membership.member, &membership.grantor] {
            let oid = i64::from(binding.oid);
            if binding.oid == 0
                || binding.object_id == [0; 16]
                || (binding.oid == 10 && binding.object_id != RoleIdentity::BOOTSTRAP.object_id)
                || self
                    .identities
                    .insert_copy(binding.object_id, oid)?
                    .is_some_and(|previous| previous != oid)
            {
                return Err(invalid(
                    "persisted role membership has an invalid role identity",
                ));
            }
        }
        if self.roles.get(&membership.grantor.identity()).is_none() {
            return Err(invalid(format!(
                "persisted role membership `{}` -> `{}` has a missing grantor identity",
                membership.member.name, membership.role.name
            )));
        }
        if membership.oid <= 0 || membership.oid > i64::from(u32::MAX) {
            return Err(invalid(format!(
                "persisted role membership has an invalid OID {}",
                membership.oid
            )));
        }
        if self.oids.insert_copy(membership.oid, ())?.is_some() {
            return Err(invalid(format!(
                "persisted role membership OID {} is duplicated",
                membership.oid
            )));
        }
        if self.keys.insert_copy(membership.key(), ())?.is_some() {
            return Err(invalid("persisted role membership identity is duplicated"));
        }
        Ok(())
    }
}

pub struct RoleMembershipValidation<'a, I> {
    active: Option<Work<'a, I>>,
    complete: bool,
}

struct Work<'a, I> {
    roles: NamedDefinitionIter<'a, RoleDefinition>,
    seeded: bool,
    memberships: I,
    checks: MembershipChecks<'a>,
    control: ProductionControl<'a>,
}

/// The original roles are indexed one per advance, followed by one membership per advance. Bound role/member incarnations may outlive their definitions; only the grantor must still resolve. Input decoding and persisted record-key checks precede this semantic boundary.
pub fn begin_membership_validation<'a, I: Iterator<Item = &'a RoleMembership>>(
    roles: &'a RoleDefinitions,
    memberships: I,
    control: ProductionControl<'a>,
) -> RoleMembershipValidation<'a, I> {
    RoleMembershipValidation {
        active: Some(Work {
            roles: roles.iter(),
            seeded: false,
            memberships,
            checks: MembershipChecks::new(control),
            control,
        }),
        complete: false,
    }
}

impl<'a, I: Iterator<Item = &'a RoleMembership>> RoleMembershipValidation<'a, I> {
    pub fn advance(&mut self) -> Result<bool, RoleRestorationError> {
        if self.complete {
            return Ok(true);
        }
        let mut work = self
            .active
            .take()
            .ok_or_else(|| invalid("role membership validation is finished"))?;
        work.control.check()?;
        if !work.seeded {
            if let Some((_, role)) = work.roles.next() {
                work.checks.role(role)?;
            } else {
                work.seeded = true;
            }
        } else if let Some(membership) = work.memberships.next() {
            work.checks.membership(membership)?;
        } else {
            self.complete = true;
            return Ok(true);
        }
        self.active = Some(work);
        Ok(false)
    }
}
