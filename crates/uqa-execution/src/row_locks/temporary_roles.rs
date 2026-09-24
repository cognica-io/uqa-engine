//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish temporary-object role references before transaction locks are released.

use super::{CrossAttachment, RowLockManager};
use std::collections::BTreeSet;
use uqa_core::CancellationToken;
use uqa_sql::SQLError;

#[cfg(test)]
mod tests;

pub(crate) const MAX_TEMPORARY_ROLE_REFERENCES: u32 = 8192;

pub(super) fn reference_limit_error() -> SQLError {
    SQLError::Routine {
        sqlstate: "53200".into(),
        message: "temporary role dependency capacity exhausted".into(),
    }
}

pub struct TemporaryRolePublication<'a> {
    manager: &'a RowLockManager,
    session: u64,
    before: BTreeSet<u32>,
    after: Option<BTreeSet<u32>>,
}

impl TemporaryRolePublication<'_> {
    pub fn commit(mut self) {
        if let Some(after) = self.after.take() {
            self.manager.finish_temporary_roles(self.session, after);
        }
    }
}

impl Drop for TemporaryRolePublication<'_> {
    fn drop(&mut self) {
        if self.after.is_some() {
            self.manager
                .finish_temporary_roles(self.session, std::mem::take(&mut self.before));
        }
    }
}

impl RowLockManager {
    pub fn has_temporary_role_references(&self, session: u64) -> bool {
        self.temporary_roles.lock().contains_key(&session)
    }

    pub fn prepare_temporary_roles<'a>(
        &'a self,
        session: u64,
        roles: BTreeSet<u32>,
        cancel: &CancellationToken,
    ) -> Result<TemporaryRolePublication<'a>, SQLError> {
        if roles.len() > MAX_TEMPORARY_ROLE_REFERENCES as usize {
            return Err(reference_limit_error());
        }
        let mut state = self.temporary_roles.lock();
        if state
            .get(&session)
            .is_some_and(|existing| *existing == roles)
            || (roles.is_empty() && !state.contains_key(&session))
        {
            return Ok(TemporaryRolePublication {
                manager: self,
                session,
                before: BTreeSet::new(),
                after: None,
            });
        }
        let before = state.get(&session).cloned().unwrap_or_default();
        let additions = roles.difference(&before).copied().collect::<Vec<_>>();
        let count = state.values().map(BTreeSet::len).sum::<usize>();
        if count.saturating_add(additions.len()) > MAX_TEMPORARY_ROLE_REFERENCES as usize {
            return Err(reference_limit_error());
        }
        match self.cross.as_ref() {
            Some(CrossAttachment::Active(coordinator)) => {
                for (index, role) in additions.iter().enumerate() {
                    if let Err(error) = coordinator.retain_temporary_role(session, *role, cancel) {
                        for added in &additions[..index] {
                            coordinator.release_temporary_role(session, *added);
                        }
                        return Err(error);
                    }
                }
            }
            Some(CrossAttachment::Unavailable(reason)) => {
                return Err(SQLError::Internal(format!(
                    "cross-process temporary role dependencies are unavailable: {reason}"
                )))
            }
            None => cancel.check()?,
        }
        state.entry(session).or_default().extend(additions);
        Ok(TemporaryRolePublication {
            manager: self,
            session,
            before,
            after: Some(roles),
        })
    }

    fn finish_temporary_roles(&self, session: u64, roles: BTreeSet<u32>) {
        let mut state = self.temporary_roles.lock();
        if let Some(previous) = state.get(&session) {
            if let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() {
                for removed in previous.difference(&roles) {
                    coordinator.release_temporary_role(session, *removed);
                }
            }
        }
        if roles.is_empty() {
            state.remove(&session);
        } else {
            state.insert(session, roles);
        }
    }

    pub fn close_temporary_roles(&self, session: u64) {
        self.finish_temporary_roles(session, BTreeSet::new());
    }

    pub fn peer_temporary_role_reference(
        &self,
        session: u64,
        role: u32,
        cancel: &CancellationToken,
    ) -> Result<bool, SQLError> {
        if self
            .temporary_roles
            .lock()
            .iter()
            .any(|(owner, roles)| *owner != session && roles.contains(&role))
        {
            return Ok(true);
        }
        match self.cross.as_ref() {
            Some(CrossAttachment::Active(coordinator)) => {
                coordinator.foreign_temporary_role_reference(role, cancel)
            }
            Some(CrossAttachment::Unavailable(reason)) => Err(SQLError::Internal(format!(
                "cross-process temporary role dependencies are unavailable: {reason}"
            ))),
            None => Ok(false),
        }
    }
}
