//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and retained object ACLs share grant closure and ordered path validation.

use super::{ObjectAclEntry, RoleIdentity, SQLError};
use crate::catalog::work::Work;
use std::collections::BTreeSet;
use uqa_core::memory::{ProductionControl, ProductionMap};

type GrantPath = (Option<RoleIdentity>, RoleIdentity);

pub(crate) trait Set<K: Copy + Ord> {
    fn contains(&self, value: &K) -> bool;
    fn insert(&mut self, value: K) -> Result<bool, SQLError>;
}

impl<K: Copy + Ord> Set<K> for BTreeSet<K> {
    fn contains(&self, value: &K) -> bool {
        BTreeSet::contains(self, value)
    }

    fn insert(&mut self, value: K) -> Result<bool, SQLError> {
        Ok(BTreeSet::insert(self, value))
    }
}

impl<K: Copy + Ord> Set<K> for ProductionMap<'_, K, ()> {
    fn contains(&self, value: &K) -> bool {
        self.get(value).is_some()
    }

    fn insert(&mut self, value: K) -> Result<bool, SQLError> {
        Ok(self.insert_copy(value, ())?.is_none())
    }
}

pub(super) async fn extend_from(
    acl: Option<&[ObjectAclEntry]>,
    reachable: &mut impl Set<RoleIdentity>,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    let Some(acl) = acl else {
        return Ok(());
    };
    loop {
        let mut changed = false;
        for entry in acl {
            work.checkpoint().await?;
            control.check()?;
            if let Some(role) = entry.role {
                if entry.grant_option && reachable.contains(&entry.grantor) {
                    changed |= reachable.insert(role)?;
                }
            }
        }
        if !changed {
            return Ok(());
        }
    }
}

pub(super) async fn validate_with<S: Set<RoleIdentity>, P: Set<GrantPath>>(
    owner: RoleIdentity,
    acl: Option<&[ObjectAclEntry]>,
    object_kind: &str,
    (mut reachable, mut paths): (S, P),
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    let invalid =
        |message: &str| SQLError::Internal(format!("invalid {object_kind} authority: {message}"));
    if !owner.is_valid() {
        return Err(invalid("missing owner incarnation"));
    }
    reachable.insert(owner)?;
    extend_from(acl, &mut reachable, control, work).await?;
    for entry in acl.into_iter().flatten() {
        work.checkpoint().await?;
        control.check()?;
        if !entry.grantor.is_valid() || entry.role.is_some_and(|role| !role.is_valid()) {
            return Err(invalid("missing ACL endpoint incarnation"));
        }
        if entry.role.is_none() && entry.grant_option {
            return Err(invalid("PUBLIC cannot retain a grant option"));
        }
        if !paths.insert((entry.role, entry.grantor))? {
            return Err(invalid("duplicate ACL grant path"));
        }
        if !reachable.contains(&entry.grantor) {
            return Err(invalid("ACL grantor has no owner-rooted grant option"));
        }
    }
    Ok(())
}

pub(crate) async fn validate_acl_from(
    owner: RoleIdentity,
    acl: Option<&[ObjectAclEntry]>,
    object_kind: &str,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    validate_with(
        owner,
        acl,
        object_kind,
        (ProductionMap::new(*control), ProductionMap::new(*control)),
        control,
        work,
    )
    .await
}

pub(crate) async fn collect_roles_from(
    owner: RoleIdentity,
    acl: Option<&[ObjectAclEntry]>,
    identities: &mut impl Set<RoleIdentity>,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    identities.insert(owner)?;
    for entry in acl.into_iter().flatten() {
        work.checkpoint().await?;
        control.check()?;
        if let Some(role) = entry.role {
            identities.insert(role)?;
        }
        identities.insert(entry.grantor)?;
    }
    Ok(())
}
