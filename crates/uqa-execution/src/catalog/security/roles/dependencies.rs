//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare role-dependent mutations without retaining registry guards across waits.

use super::locking::{RoleDependencyLocks, RoleLockContext};
use std::collections::BTreeSet;
use uqa_sql::{
    catalog::roles::guards::{RoleDefinitionRead, RoleMembershipRead},
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
    mut prepare_writer: impl FnMut() -> Result<(), SQLError>,
    mut prepare: impl FnMut() -> Result<RoleDependencyCandidate<'a, T>, SQLError>,
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
        } else if writer_prepared {
            return Ok(candidate);
        } else {
            drop(candidate);
            prepare_writer()?;
            writer_prepared = true;
        }
    }
}
