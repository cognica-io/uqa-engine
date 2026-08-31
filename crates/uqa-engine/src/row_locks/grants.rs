//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-process row grant table and tuple-lock conflict rules.

use super::{
    AtomicU64, ByteClaim, CommittedRowChange, HashMap, LockStrength, Ordering, RelationLockGrant,
    RelationLockMode, RowLockKey,
};

#[derive(Clone, Debug)]
pub(super) struct LockGrant {
    pub(super) session_id: u64,
    pub(super) acquisitions: Vec<MarkedStrength>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MarkedStrength {
    pub(super) acquisition_id: u64,
    pub(super) strength: LockStrength,
    pub(super) mark: u32,
}

impl LockGrant {
    pub(super) fn effective_strength(&self) -> LockStrength {
        self.acquisitions
            .iter()
            .map(|acquisition| acquisition.strength)
            .max()
            .expect("row-lock grant must retain an acquisition")
    }
}

pub(super) struct LockTable {
    pub(super) rows: HashMap<RowLockKey, Vec<LockGrant>>,
    pub(super) waiting: HashMap<u64, HashMap<RowLockKey, LockStrength>>,
    pub(super) relations: HashMap<u64, Vec<RelationLockGrant>>,
    pub(super) waiting_relations: HashMap<u64, HashMap<u64, RelationLockMode>>,
    /// The sidecar byte each local session is currently blocked on, whether its immediate holder is local or foreign. Publishing local edges lets another process walk mixed local/cross-process deadlock cycles.
    pub(super) advertised_waits: HashMap<u64, ByteClaim>,
    pub(super) changes: Vec<CommittedRowChange>,
    pub(super) change_epoch: u64,
    pub(super) active_change_observers: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockAcquire {
    Granted {
        waited: bool,
        /// Whether this acquisition waited for a conflicting holder in another OS process. Cross-process commits are invisible to the in-process change epochs, so the lock consumer rechecks such candidates against the latest committed row images.
        foreign_waited: bool,
        acquisition: Option<RowLockAcquisition>,
    },
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowLockAcquisition {
    pub(super) session_id: u64,
    pub(super) key: RowLockKey,
    pub(super) acquisition_id: u64,
}

pub(super) enum GrantAttempt {
    Conflict,
    Granted(Option<RowLockAcquisition>),
}

pub(crate) struct LockRequest<'a> {
    pub session_id: u64,
    pub key: RowLockKey,
    pub strength: LockStrength,
    pub mark: u32,
    pub wait: uqa_sql::ast::LockWait,
    pub cancel: &'a uqa_core::CancellationToken,
    pub relation: &'a str,
}

/// Undo one just-granted row acquisition whose cross-process claim failed.
pub(super) fn rollback_grant(state: &mut LockTable, acquisition: RowLockAcquisition) {
    if let Some(grants) = state.rows.get_mut(&acquisition.key) {
        grants.retain_mut(|grant| {
            if grant.session_id == acquisition.session_id {
                grant
                    .acquisitions
                    .retain(|marked| marked.acquisition_id != acquisition.acquisition_id);
            }
            !grant.acquisitions.is_empty()
        });
        if grants.is_empty() {
            state.rows.remove(&acquisition.key);
        }
    }
}

pub(super) fn try_grant(
    state: &mut LockTable,
    session_id: u64,
    key: RowLockKey,
    strength: LockStrength,
    mark: u32,
    next_acquisition: &AtomicU64,
) -> GrantAttempt {
    let grants = state.rows.entry(key).or_default();
    if grants.iter().any(|grant| {
        grant.session_id != session_id
            && lock_strengths_conflict(grant.effective_strength(), strength)
    }) {
        return GrantAttempt::Conflict;
    }
    if let Some(existing) = grants
        .iter_mut()
        .find(|grant| grant.session_id == session_id)
    {
        if strength <= existing.effective_strength() {
            return GrantAttempt::Granted(None);
        }
        let acquisition_id = next_acquisition.fetch_add(1, Ordering::Relaxed);
        existing.acquisitions.push(MarkedStrength {
            acquisition_id,
            strength,
            mark,
        });
        return GrantAttempt::Granted(Some(RowLockAcquisition {
            session_id,
            key,
            acquisition_id,
        }));
    }
    let acquisition_id = next_acquisition.fetch_add(1, Ordering::Relaxed);
    grants.push(LockGrant {
        session_id,
        acquisitions: vec![MarkedStrength {
            acquisition_id,
            strength,
            mark,
        }],
    });
    GrantAttempt::Granted(Some(RowLockAcquisition {
        session_id,
        key,
        acquisition_id,
    }))
}

pub(crate) fn lock_strengths_conflict(left: LockStrength, right: LockStrength) -> bool {
    if left == LockStrength::ForUpdate || right == LockStrength::ForUpdate {
        return true;
    }
    if left == LockStrength::ForKeyShare || right == LockStrength::ForKeyShare {
        return false;
    }
    matches!(
        (left, right),
        (
            LockStrength::ForShare | LockStrength::ForNoKeyUpdate,
            LockStrength::ForNoKeyUpdate
        ) | (LockStrength::ForNoKeyUpdate, LockStrength::ForShare)
    )
}
