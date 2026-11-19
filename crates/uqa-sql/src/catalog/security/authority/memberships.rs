//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Topological membership validation uses admitted adjacency and one queue entry per role.

use super::{
    invalid, valid_identity, AuthorityCatalog, RoleIdentity, RoleMemberships, SQLError, Work,
};
use uqa_core::memory::{BudgetedMap, BudgetedVec};

struct Edge {
    role: RoleIdentity,
    next: Option<usize>,
}

pub(super) async fn validate(
    catalog: &AuthorityCatalog<'_>,
    memberships: &RoleMemberships,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    let mut degree = BudgetedMap::new(catalog.memory);
    let mut heads = BudgetedMap::new(catalog.memory);
    let mut edges = BudgetedVec::new(catalog.memory);
    let mut identities = BudgetedMap::new(catalog.memory);
    let mut membership_oids = BudgetedMap::new(catalog.memory);
    for (_, role) in &catalog.roles {
        catalog.checkpoint(work).await?;
        identities.insert(role.object_id, role.oid)?;
    }
    for (key, membership) in memberships {
        catalog.checkpoint(work).await?;
        let role = membership.role.identity();
        let member = membership.member.identity();
        for endpoint in [role, member, membership.grantor.identity()] {
            if !valid_identity(endpoint)
                || identities
                    .insert(endpoint.object_id, endpoint.oid)?
                    .is_some_and(|oid| oid != endpoint.oid)
            {
                return Err(invalid("invalid retained membership incarnation"));
            }
        }
        catalog.role(membership.grantor.identity())?;
        if *key != membership.key()
            || role == member
            || membership.oid <= 0
            || u32::try_from(membership.oid).is_err()
            || membership_oids.insert(membership.oid, ())?.is_some()
        {
            return Err(invalid("invalid role membership edge"));
        }
        let count = degree.get(&role).copied().unwrap_or(0usize);
        degree.insert(
            role,
            count
                .checked_add(1)
                .ok_or_else(|| invalid("membership count"))?,
        )?;
        if degree.get(&member).is_none() {
            degree.insert(member, 0)?;
        }
        let index = edges.len();
        edges.push(Edge {
            role,
            next: heads.get(&member).copied(),
        })?;
        heads.insert(member, index)?;
    }
    let mut pending = BudgetedVec::new(catalog.memory);
    for (identity, count) in &degree {
        catalog.checkpoint(work).await?;
        if *count == 0 {
            pending.push(*identity)?;
        }
    }
    let mut remaining = degree.len();
    while let Some(identity) = pending.pop() {
        catalog.checkpoint(work).await?;
        remaining -= 1;
        let mut next = heads.get(&identity).copied();
        while let Some(index) = next {
            catalog.checkpoint(work).await?;
            let edge = &edges[index];
            let count = degree
                .get_mut(&edge.role)
                .expect("admitted membership target");
            *count -= 1;
            if *count == 0 {
                pending.push(edge.role)?;
            }
            next = edge.next;
        }
    }
    if remaining != 0 {
        return Err(invalid("cyclic role memberships"));
    }
    let control = catalog.control();
    crate::catalog::work::retire_entries(membership_oids.into_entries(), &control, work).await?;
    crate::catalog::work::retire_entries(identities.into_entries(), &control, work).await?;
    crate::catalog::work::retire_entries(heads.into_entries(), &control, work).await?;
    crate::catalog::work::retire_entries(degree.into_entries(), &control, work).await?;
    Ok(())
}
