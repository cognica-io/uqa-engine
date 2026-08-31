//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-lock grants and acquisition lifecycle.

use super::{
    deadlock_detected, relation_byte_claims, relation_deadlock_exists, CrossAttachment,
    CrossWaitGuard, LockTable, RowLockManager, SQLError, WAIT_SLICE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RelationLockMode {
    AccessShare,
    RowShare,
    RowExclusive,
    AccessExclusive,
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

impl RelationLockGrant {
    pub(super) fn effective_mode(&self) -> RelationLockMode {
        self.acquisitions
            .iter()
            .map(|acquisition| acquisition.mode)
            .max()
            .expect("relation-lock grant must retain an acquisition")
    }
}

impl RowLockManager {
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

    pub(crate) fn acquire_relation(
        &self,
        session_id: u64,
        table: u64,
        mode: RelationLockMode,
        mark: u32,
        cancel: &uqa_core::CancellationToken,
    ) -> Result<(), SQLError> {
        let coordinator = self.coordinator()?;
        let relation = self.relation_bytes(table);
        let claims = coordinator
            .map(|_| relation_byte_claims(&relation, mode))
            .unwrap_or_default();
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
                        coordinator.and_then(|_| claims.first().copied())
                    }
                    RelationGrantAttempt::AlreadyHeld => {
                        state.waiting_relations.remove(&session_id);
                        return Ok(());
                    }
                    RelationGrantAttempt::Granted => {
                        let foreign_conflict = match coordinator {
                            Some(coordinator) => match coordinator.try_claim(session_id, &claims) {
                                Ok(Ok(())) => None,
                                Ok(Err(contended)) => Some(contended),
                                Err(error) => {
                                    rollback_relation_grant(&mut state, session_id, table);
                                    drop(state);
                                    self.wake.notify_all();
                                    return Err(SQLError::Internal(error));
                                }
                            },
                            None => None,
                        };
                        match foreign_conflict {
                            None => {
                                state.waiting_relations.remove(&session_id);
                                return Ok(());
                            }
                            Some(contended) => {
                                rollback_relation_grant(&mut state, session_id, table);
                                Some(contended)
                            }
                        }
                    }
                };
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
    if grants.iter().any(|grant| {
        grant.session_id != session_id && relation_modes_conflict(grant.effective_mode(), mode)
    }) {
        return RelationGrantAttempt::Conflict;
    }
    if let Some(existing) = grants
        .iter_mut()
        .find(|grant| grant.session_id == session_id)
    {
        if mode <= existing.effective_mode() {
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

pub(super) fn relation_modes_conflict(left: RelationLockMode, right: RelationLockMode) -> bool {
    left == RelationLockMode::AccessExclusive || right == RelationLockMode::AccessExclusive
}
