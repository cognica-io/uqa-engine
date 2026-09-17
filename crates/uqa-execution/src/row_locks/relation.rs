//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-lock grants and acquisition lifecycle.

use super::cross_process::{relation_wait_claim, RelationClaimWait};
use super::{
    deadlock_detected, relation_byte_claims, relation_deadlock_exists, CrossAttachment,
    CrossWaitGuard, LockTable, RowLockManager, SQLError, WAIT_SLICE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum RelationLockMode {
    AccessShare,
    RowShare,
    RowExclusive,
    ShareUpdateExclusive,
    Share,
    ShareRowExclusive,
    Exclusive,
    AccessExclusive,
}

impl RelationLockMode {
    #[cfg(any(test, windows, all(unix, not(target_os = "emscripten"))))]
    pub(super) const ALL: [Self; 8] = [
        Self::AccessShare,
        Self::RowShare,
        Self::RowExclusive,
        Self::ShareUpdateExclusive,
        Self::Share,
        Self::ShareRowExclusive,
        Self::Exclusive,
        Self::AccessExclusive,
    ];

    /// The `PostgreSQL` table-lock conflict sets are not an ordering of strengths.
    pub fn conflicts_with(self, other: Self) -> bool {
        const CONFLICTS: [u8; 8] = [0x80, 0xc0, 0xf0, 0xf8, 0xec, 0xfc, 0xfe, 0xff];
        CONFLICTS[self as usize] & (1 << other as u8) != 0
    }
}

/// A temporary acquisition above a caller-owned mark. The session must not acquire unrelated locks until this guard is dropped.
pub struct ScopedRelationLock<'a> {
    manager: &'a RowLockManager,
    session_id: u64,
    table: u64,
    keep_mark: u32,
    retained: bool,
}

impl ScopedRelationLock<'_> {
    /// Keep the successful binding acquisition in the caller's transaction/savepoint after catalog revalidation. No unrelated locks may be acquired while the temporary guard is alive.
    pub fn retain(mut self) {
        let mut state = self.manager.state.lock();
        if let Some(grant) = state.relations.get_mut(&self.table).and_then(|grants| {
            grants
                .iter_mut()
                .find(|grant| grant.session_id == self.session_id)
        }) {
            for acquisition in &mut grant.acquisitions {
                acquisition.mark = acquisition.mark.min(self.keep_mark);
            }
        }
        self.retained = true;
    }
}

impl Drop for ScopedRelationLock<'_> {
    fn drop(&mut self) {
        if !self.retained {
            self.manager
                .release_mark_above(self.session_id, self.keep_mark);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MarkedRelationMode {
    pub(super) mode: RelationLockMode,
    pub(super) mark: u32,
}

#[derive(Clone, Debug)]
pub(super) struct RelationLockGrant {
    pub(super) session_id: u64,
    pub(super) acquisitions: Vec<MarkedRelationMode>,
}

struct RelationRequest<'a> {
    session_id: u64,
    table: u64,
    mode: RelationLockMode,
    mark: u32,
    cancel: &'a uqa_core::CancellationToken,
}

impl RelationLockGrant {
    pub(super) fn conflicting_mode(&self, requested: RelationLockMode) -> Option<RelationLockMode> {
        self.acquisitions
            .iter()
            .map(|acquisition| acquisition.mode)
            .find(|mode| mode.conflicts_with(requested))
    }
}

impl RowLockManager {
    pub fn acquire_scoped_relation(
        &self,
        session_id: u64,
        table: u64,
        mode: RelationLockMode,
        marks: (u32, u32),
        cancel: &uqa_core::CancellationToken,
    ) -> Result<ScopedRelationLock<'_>, SQLError> {
        let (keep_mark, mark) = marks;
        if mark <= keep_mark {
            return Err(SQLError::Internal(
                "temporary relation lock requires a newer mark".into(),
            ));
        }
        self.acquire_relation(session_id, table, mode, mark, cancel)?;
        Ok(ScopedRelationLock {
            manager: self,
            session_id,
            table,
            keep_mark,
            retained: false,
        })
    }

    pub fn try_acquire_scoped_relation(
        &self,
        session_id: u64,
        table: u64,
        mode: RelationLockMode,
        marks: (u32, u32),
        cancel: &uqa_core::CancellationToken,
    ) -> Result<Option<ScopedRelationLock<'_>>, SQLError> {
        let (keep_mark, mark) = marks;
        if mark <= keep_mark {
            return Err(SQLError::Internal(
                "temporary relation lock requires a newer mark".into(),
            ));
        }
        Ok(self
            .try_acquire_relation(session_id, table, mode, mark, cancel)?
            .then(|| ScopedRelationLock {
                manager: self,
                session_id,
                table,
                keep_mark,
                retained: false,
            }))
    }

    pub(super) fn release_relation_claims(
        &self,
        session_id: u64,
        table: u64,
        mode: RelationLockMode,
    ) {
        if let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() {
            let relation = self.relation_bytes(table);
            coordinator.release(session_id, &relation_byte_claims(&relation, mode));
        }
    }

    pub fn acquire_relation(
        &self,
        session_id: u64,
        table: u64,
        mode: RelationLockMode,
        mark: u32,
        cancel: &uqa_core::CancellationToken,
    ) -> Result<(), SQLError> {
        self.acquire_relation_inner(
            RelationRequest {
                session_id,
                table,
                mode,
                mark,
                cancel,
            },
            true,
        )
        .map(|_| ())
    }

    /// Acquire a relation lock without waiting for conflicting holders. Short internal admission serialization is independent of the SQL wait policy; a failed attempt preserves this session's earlier modes and savepoint marks.
    pub fn try_acquire_relation(
        &self,
        session_id: u64,
        table: u64,
        mode: RelationLockMode,
        mark: u32,
        cancel: &uqa_core::CancellationToken,
    ) -> Result<bool, SQLError> {
        self.acquire_relation_inner(
            RelationRequest {
                session_id,
                table,
                mode,
                mark,
                cancel,
            },
            false,
        )
    }

    fn acquire_relation_inner(
        &self,
        request: RelationRequest<'_>,
        wait: bool,
    ) -> Result<bool, SQLError> {
        let RelationRequest {
            session_id,
            table,
            mode,
            mark,
            cancel,
        } = request;
        let coordinator = self.coordinator()?;
        let relation = self.relation_bytes(table);
        let cross_wait = CrossWaitGuard::new(self, coordinator, session_id);
        loop {
            let mut state = self.state.lock();
            if let Err(error) = cancel.check() {
                state.waiting_relations.remove(&session_id);
                drop(state);
                self.wake.notify_all();
                return Err(error.into());
            }
            let contended_claim =
                match try_grant_relation(&mut state, session_id, table, mode, mark) {
                    RelationGrantAttempt::Conflict => {
                        coordinator.map(|_| relation_wait_claim(&relation, mode))
                    }
                    RelationGrantAttempt::AlreadyHeld => {
                        state.waiting_relations.remove(&session_id);
                        return Ok(true);
                    }
                    RelationGrantAttempt::Granted => {
                        let foreign_conflict = match coordinator {
                            Some(coordinator) => {
                                match coordinator.try_relation_claim(session_id, &relation, mode) {
                                    Ok(Ok(())) => None,
                                    Ok(Err(RelationClaimWait::Conflict(_))) => {
                                        Some(relation_wait_claim(&relation, mode))
                                    }
                                    Ok(Err(RelationClaimWait::AdmissionBusy)) => {
                                        rollback_relation_grant(&mut state, session_id, table);
                                        state.waiting_relations.remove(&session_id);
                                        cross_wait.clear(&mut state);
                                        self.wake.wait_for(&mut state, WAIT_SLICE);
                                        continue;
                                    }
                                    Err(error) => {
                                        rollback_relation_grant(&mut state, session_id, table);
                                        drop(state);
                                        self.wake.notify_all();
                                        return Err(SQLError::Internal(error));
                                    }
                                }
                            }
                            None => None,
                        };
                        match foreign_conflict {
                            None => {
                                state.waiting_relations.remove(&session_id);
                                return Ok(true);
                            }
                            Some(contended) => {
                                rollback_relation_grant(&mut state, session_id, table);
                                Some(contended)
                            }
                        }
                    }
                };
            if !wait {
                state.waiting_relations.remove(&session_id);
                drop(state);
                self.wake.notify_all();
                return Ok(false);
            }
            if relation_deadlock_exists(&state, session_id, table, mode) {
                state.waiting_relations.remove(&session_id);
                drop(state);
                self.wake.notify_all();
                return Err(deadlock_detected());
            }
            if let (Some(coordinator), Some(contended)) = (coordinator, contended_claim) {
                cross_wait.register(&mut state, contended);
                if Self::cross_wait_cycle(&state, coordinator, session_id, contended) {
                    state.waiting_relations.remove(&session_id);
                    state.advertised_waits.remove(&session_id);
                    drop(state);
                    self.wake.notify_all();
                    return Err(deadlock_detected());
                }
            }
            state
                .waiting_relations
                .entry(session_id)
                .or_default()
                .insert(table, mode);
            self.wake.wait_for(&mut state, WAIT_SLICE);
        }
    }
}

/// Undo one just-granted relation acquisition whose cross-process claim failed: the newest acquisition of this session on the table.
fn rollback_relation_grant(state: &mut LockTable, session_id: u64, table: u64) {
    if let Some(grants) = state.relations.get_mut(&table) {
        grants.retain_mut(|grant| {
            if grant.session_id == session_id {
                grant.acquisitions.pop();
            }
            !grant.acquisitions.is_empty()
        });
        if grants.is_empty() {
            state.relations.remove(&table);
        }
    }
}

enum RelationGrantAttempt {
    Conflict,
    AlreadyHeld,
    Granted,
}

fn try_grant_relation(
    state: &mut LockTable,
    session_id: u64,
    table: u64,
    mode: RelationLockMode,
    mark: u32,
) -> RelationGrantAttempt {
    let grants = state.relations.entry(table).or_default();
    if grants
        .iter()
        .any(|grant| grant.session_id != session_id && grant.conflicting_mode(mode).is_some())
    {
        return RelationGrantAttempt::Conflict;
    }
    if let Some(existing) = grants
        .iter_mut()
        .find(|grant| grant.session_id == session_id)
    {
        if existing
            .acquisitions
            .iter()
            .any(|acquisition| acquisition.mode == mode)
        {
            return RelationGrantAttempt::AlreadyHeld;
        }
        existing
            .acquisitions
            .push(MarkedRelationMode { mode, mark });
        return RelationGrantAttempt::Granted;
    }
    grants.push(RelationLockGrant {
        session_id,
        acquisitions: vec![MarkedRelationMode { mode, mark }],
    });
    RelationGrantAttempt::Granted
}

#[cfg(test)]
mod tests;
