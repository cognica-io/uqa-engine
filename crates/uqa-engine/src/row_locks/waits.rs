//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-lock waits, cross-process claims, and deadlock detection.

use super::{
    lock_strengths_conflict, relation_modes_conflict, remove_inactive_versions, rollback_grant,
    row_byte_claims, try_grant, ByteClaim, CrossAttachment, FileLockCoordinator, GrantAttempt,
    HashSet, LockAcquire, LockRequest, LockStrength, LockTable, MutexGuard, RelationLockMode,
    RowLockAcquisition, RowLockKey, RowLockManager, SQLError, WAIT_SLICE,
};

impl RowLockManager {
    pub(super) fn release_row_claims(
        &self,
        session_id: u64,
        key: RowLockKey,
        strength: LockStrength,
    ) {
        if let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() {
            let relation = self.relation_bytes(key.table);
            coordinator.release(
                session_id,
                &row_byte_claims(&relation, key.doc_id, strength),
            );
        }
    }

    /// Whether the cross-process wait-for graph from `wanted` closes back on `session_id`. Bytes held by other local sessions are followed through those sessions' own foreign waits.
    pub(super) fn cross_wait_cycle(
        state: &LockTable,
        coordinator: &FileLockCoordinator,
        session_id: u64,
        wanted: ByteClaim,
    ) -> bool {
        let local_wait = |session: u64| state.advertised_waits.get(&session).copied();
        coordinator.wait_cycle_reaches_session(session_id, wanted, &local_wait)
    }

    fn finish_row_wait(&self, mut state: MutexGuard<'_, LockTable>, session_id: u64) {
        state.waiting.remove(&session_id);
        state.advertised_waits.remove(&session_id);
        remove_inactive_versions(&mut state);
        drop(state);
        self.wake.notify_all();
    }

    pub(crate) fn acquire(&self, request: &LockRequest<'_>) -> Result<LockAcquire, SQLError> {
        let coordinator = self.coordinator()?;
        let relation = self.relation_bytes(request.key.table);
        let claims = coordinator
            .map(|_| row_byte_claims(&relation, request.key.doc_id, request.strength))
            .unwrap_or_default();
        let cross_wait = CrossWaitGuard::new(self, coordinator, request.session_id);
        let mut waited = false;
        let mut foreign_waited = false;
        loop {
            let mut state = self.state.lock();
            if let Err(error) = request.cancel.check() {
                self.finish_row_wait(state, request.session_id);
                return Err(error.into());
            }
            let attempt = try_grant(
                &mut state,
                request.session_id,
                request.key,
                request.strength,
                request.mark,
                &self.next_acquisition,
            );
            let contended_claim = match attempt {
                GrantAttempt::Granted(acquisition) => {
                    match claim_cross_process_bytes(&mut state, coordinator, &claims, acquisition) {
                        Ok(None) => {
                            state.waiting.remove(&request.session_id);
                            remove_inactive_versions(&mut state);
                            return Ok(LockAcquire::Granted {
                                waited,
                                foreign_waited,
                                acquisition,
                            });
                        }
                        Ok(Some(contended)) => {
                            foreign_waited = true;
                            Some(contended)
                        }
                        Err(error) => {
                            self.finish_row_wait(state, request.session_id);
                            return Err(error);
                        }
                    }
                }
                GrantAttempt::Conflict => coordinator.and_then(|_| {
                    locally_contended_row_claim(
                        &state,
                        request.session_id,
                        request.key,
                        &relation,
                        &claims,
                    )
                }),
            };
            match request.wait {
                uqa_sql::ast::LockWait::SkipLocked => {
                    self.finish_row_wait(state, request.session_id);
                    return Ok(LockAcquire::Skipped);
                }
                uqa_sql::ast::LockWait::NoWait => {
                    self.finish_row_wait(state, request.session_id);
                    return Err(lock_unavailable(request.relation));
                }
                uqa_sql::ast::LockWait::Block => {
                    if deadlock_exists(&state, request.session_id, request.key, request.strength) {
                        self.finish_row_wait(state, request.session_id);
                        return Err(deadlock_detected());
                    }
                    if let (Some(coordinator), Some(contended)) = (coordinator, contended_claim) {
                        cross_wait.register(&mut state, contended);
                        if Self::cross_wait_cycle(
                            &state,
                            coordinator,
                            request.session_id,
                            contended,
                        ) {
                            self.finish_row_wait(state, request.session_id);
                            return Err(deadlock_detected());
                        }
                    }
                    state
                        .waiting
                        .entry(request.session_id)
                        .or_default()
                        .insert(request.key, request.strength);
                    waited = true;
                    self.wake.wait_for(&mut state, WAIT_SLICE);
                }
            }
        }
    }
}

pub(super) fn deadlock_detected() -> SQLError {
    SQLError::Routine {
        sqlstate: "40P01".into(),
        message: "deadlock detected".into(),
    }
}

/// Advertises one session's cross-process wait for the duration of an acquisition and clears it on every exit path.
pub(super) struct CrossWaitGuard<'a> {
    manager: &'a RowLockManager,
    coordinator: Option<&'a FileLockCoordinator>,
    session_id: u64,
    registered: std::cell::Cell<bool>,
}

impl<'a> CrossWaitGuard<'a> {
    pub(super) fn new(
        manager: &'a RowLockManager,
        coordinator: Option<&'a FileLockCoordinator>,
        session_id: u64,
    ) -> Self {
        Self {
            manager,
            coordinator,
            session_id,
            registered: std::cell::Cell::new(false),
        }
    }

    pub(super) fn register(&self, state: &mut LockTable, claim: ByteClaim) {
        if let Some(coordinator) = self.coordinator {
            coordinator.register_wait(self.session_id, claim);
            state.advertised_waits.insert(self.session_id, claim);
            self.registered.set(true);
        }
    }
}

impl Drop for CrossWaitGuard<'_> {
    fn drop(&mut self) {
        if self.registered.get() {
            if let Some(coordinator) = self.coordinator {
                coordinator.clear_wait(self.session_id);
            }
            self.manager
                .state
                .lock()
                .advertised_waits
                .remove(&self.session_id);
        }
    }
}

/// Add the cross-process record-lock claims for one just-granted row acquisition. Only a new acquisition adds claims: re-acquiring an equal-or-weaker strength changes nothing another process could observe. A contended claim rolls the in-process grant back and reports the byte to wait on; an infrastructure failure rolls back and surfaces the error.
fn claim_cross_process_bytes(
    state: &mut LockTable,
    coordinator: Option<&FileLockCoordinator>,
    claims: &[ByteClaim],
    acquisition: Option<RowLockAcquisition>,
) -> Result<Option<ByteClaim>, SQLError> {
    let (Some(coordinator), Some(new_acquisition)) = (coordinator, acquisition) else {
        return Ok(None);
    };
    match coordinator.try_claim(new_acquisition.session_id, claims) {
        Ok(Ok(())) => Ok(None),
        Ok(Err(contended)) => {
            rollback_grant(state, new_acquisition);
            Ok(Some(contended))
        }
        Err(error) => {
            rollback_grant(state, new_acquisition);
            Err(SQLError::Internal(error))
        }
    }
}

fn locally_contended_row_claim(
    state: &LockTable,
    session_id: u64,
    key: RowLockKey,
    relation: &[u8],
    wanted_claims: &[ByteClaim],
) -> Option<ByteClaim> {
    state.rows.get(&key)?.iter().find_map(|grant| {
        if grant.session_id == session_id {
            return None;
        }
        row_byte_claims(relation, key.doc_id, grant.effective_strength())
            .into_iter()
            .find_map(|held| {
                wanted_claims
                    .iter()
                    .copied()
                    .find(|wanted| byte_claims_conflict(*wanted, held))
            })
    })
}

fn byte_claims_conflict(wanted: ByteClaim, held: ByteClaim) -> bool {
    wanted.offset == held.offset && (wanted.write || held.write)
}

pub(super) fn deadlock_exists(
    state: &LockTable,
    waiter: u64,
    wanted: RowLockKey,
    wanted_strength: LockStrength,
) -> bool {
    wait_cycle_reaches(
        state,
        waiter,
        holders_of(state, wanted, waiter, wanted_strength),
    )
}

pub(super) fn relation_deadlock_exists(
    state: &LockTable,
    waiter: u64,
    table: u64,
    mode: RelationLockMode,
) -> bool {
    wait_cycle_reaches(
        state,
        waiter,
        relation_holders_of(state, table, waiter, mode),
    )
}

fn wait_cycle_reaches(state: &LockTable, waiter: u64, mut stack: Vec<u64>) -> bool {
    let mut seen = HashSet::from([waiter]);
    while let Some(session) = stack.pop() {
        if !seen.insert(session) {
            continue;
        }
        if let Some(waiting_for) = state.waiting.get(&session) {
            for (key, strength) in waiting_for {
                for holder in holders_of(state, *key, session, *strength) {
                    if holder == waiter {
                        return true;
                    }
                    stack.push(holder);
                }
            }
        }
        if let Some(waiting_for) = state.waiting_relations.get(&session) {
            for (table, mode) in waiting_for {
                for holder in relation_holders_of(state, *table, session, *mode) {
                    if holder == waiter {
                        return true;
                    }
                    stack.push(holder);
                }
            }
        }
    }
    false
}

fn relation_holders_of(
    state: &LockTable,
    table: u64,
    except: u64,
    wanted_mode: RelationLockMode,
) -> Vec<u64> {
    state
        .relations
        .get(&table)
        .into_iter()
        .flatten()
        .filter(|grant| {
            grant.session_id != except
                && relation_modes_conflict(grant.effective_mode(), wanted_mode)
        })
        .map(|grant| grant.session_id)
        .collect()
}

fn holders_of(
    state: &LockTable,
    key: RowLockKey,
    except: u64,
    wanted_strength: LockStrength,
) -> Vec<u64> {
    state
        .rows
        .get(&key)
        .into_iter()
        .flatten()
        .filter(|grant| {
            grant.session_id != except
                && lock_strengths_conflict(grant.effective_strength(), wanted_strength)
        })
        .map(|grant| grant.session_id)
        .collect()
}

pub(crate) fn lock_unavailable(relation: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "55P03".into(),
        message: format!("could not obtain lock on row in relation \"{relation}\""),
    }
}
