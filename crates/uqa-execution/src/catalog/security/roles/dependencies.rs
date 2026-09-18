//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare role-dependent mutations without retaining registry guards across waits.

use super::locking::{RoleBinding, RoleDependencyLocks, RoleLockContext};
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::{
    catalog::roles::{
        guards::{RoleDefinitionRead, RoleMembershipRead},
        RoleDefinition, RoleMembership, RoleMembershipKey,
    },
    SQLError,
};

/// Object registry guards in the value release before memberships and role definitions when preflight is discarded.
pub struct RoleDependencyCandidate<'a, T> {
    pub value: T,
    pub memberships: RoleMembershipRead<'a>,
    pub roles: RoleDefinitionRead<'a>,
    pub dependencies: BTreeSet<String>,
}

#[cfg(test)]
mod tests;

pub fn prepare_role_dependencies<'a, T>(
    context: &RoleLockContext<'_>,
    prepare_writer: impl FnMut() -> Result<(), SQLError>,
    prepare: impl FnMut() -> Result<RoleDependencyCandidate<'a, T>, SQLError>,
) -> Result<RoleDependencyCandidate<'a, T>, SQLError> {
    prepare_dependencies(context, prepare_writer, prepare, |_| true)
}

/// Bind the requested owner before the caller's relevant waits and retain that incarnation through publication.
pub fn prepare_role_owner<'a, T>(
    context: RoleLockContext<'a>,
    owner: &RoleBinding,
    prepare_writer: impl FnMut() -> Result<(), SQLError>,
    mut prepare: impl FnMut(
        &BTreeMap<String, RoleDefinition>,
        &BTreeMap<RoleMembershipKey, RoleMembership>,
    ) -> Result<Option<T>, SQLError>,
) -> Result<RoleDependencyCandidate<'a, Option<T>>, SQLError> {
    prepare_dependencies(
        &context,
        prepare_writer,
        || {
            let roles = context.roles.role_definitions();
            owner.revalidate(&roles)?;
            let memberships = context.roles.role_memberships();
            let value = prepare(&roles, &memberships)?;
            let dependencies = if value.is_some() {
                BTreeSet::from([owner.name.clone()])
            } else {
                BTreeSet::new()
            };
            Ok(RoleDependencyCandidate {
                value,
                memberships,
                roles,
                dependencies,
            })
        },
        Option::is_some,
    )
}

fn prepare_dependencies<'a, T>(
    context: &RoleLockContext<'_>,
    mut prepare_writer: impl FnMut() -> Result<(), SQLError>,
    mut prepare: impl FnMut() -> Result<RoleDependencyCandidate<'a, T>, SQLError>,
    needs_writer: impl Fn(&T) -> bool,
) -> Result<RoleDependencyCandidate<'a, T>, SQLError> {
    let mut held = RoleDependencyLocks::default();
    let mut writer_prepared = false;
    loop {
        let candidate = prepare()?;
        let pending = held.missing(&candidate.roles, &candidate.dependencies)?;
        if !pending.is_empty() {
            drop(candidate);
            held.acquire(context, pending)?;
            writer_prepared = false;
        } else if writer_prepared || !needs_writer(&candidate.value) {
            return Ok(candidate);
        } else {
            drop(candidate);
            prepare_writer()?;
            writer_prepared = true;
        }
    }
}
