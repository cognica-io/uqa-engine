//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process-wide logical row locks for `FOR UPDATE` / `FOR SHARE`.
//!
//! Locks are in-memory and follow `PostgreSQL` 18 tuple-lock conflict rules.
//! They are held until the owning session's transaction ends or a savepoint
//! rolls back the acquisition.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use uqa_core::DocId;
use uqa_sql::ast::LockStrength;
use uqa_sql::SQLError;

const WAIT_SLICE: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RowLockKey {
    pub table: u64,
    pub doc_id: DocId,
}

#[derive(Clone, Copy, Debug)]
struct LockGrant {
    session_id: u64,
    strength: LockStrength,
    mark: u32,
}

struct LockTable {
    rows: HashMap<RowLockKey, Vec<LockGrant>>,
    waiting: HashMap<u64, HashSet<RowLockKey>>,
}

pub(crate) struct RowLockManager {
    next_session: AtomicU64,
    table_ids: Mutex<HashMap<String, u64>>,
    next_table: AtomicU64,
    state: Mutex<LockTable>,
    wake: Condvar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockAcquire {
    Granted,
    Skipped,
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

impl RowLockManager {
    pub(crate) fn new() -> Self {
        Self {
            next_session: AtomicU64::new(1),
            table_ids: Mutex::new(HashMap::new()),
            next_table: AtomicU64::new(1),
            state: Mutex::new(LockTable {
                rows: HashMap::new(),
                waiting: HashMap::new(),
            }),
            wake: Condvar::new(),
        }
    }

    pub(crate) fn allocate_session(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn table_key(&self, table: &str) -> u64 {
        let mut tables = self.table_ids.lock();
        if let Some(id) = tables.get(table) {
            return *id;
        }
        let id = self.next_table.fetch_add(1, Ordering::Relaxed);
        tables.insert(table.to_string(), id);
        id
    }

    pub(crate) fn acquire(&self, request: &LockRequest<'_>) -> Result<LockAcquire, SQLError> {
        loop {
            request.cancel.check()?;
            let mut state = self.state.lock();
            if try_grant(
                &mut state,
                request.session_id,
                request.key,
                request.strength,
                request.mark,
            ) {
                state.waiting.remove(&request.session_id);
                return Ok(LockAcquire::Granted);
            }
            match request.wait {
                uqa_sql::ast::LockWait::SkipLocked => {
                    state.waiting.remove(&request.session_id);
                    return Ok(LockAcquire::Skipped);
                }
                uqa_sql::ast::LockWait::NoWait => {
                    state.waiting.remove(&request.session_id);
                    return Err(lock_unavailable(request.relation));
                }
                uqa_sql::ast::LockWait::Block => {
                    if deadlock_exists(&state, request.session_id, request.key) {
                        state.waiting.remove(&request.session_id);
                        return Err(SQLError::Routine {
                            sqlstate: "40P01".into(),
                            message: "deadlock detected".into(),
                        });
                    }
                    state
                        .waiting
                        .entry(request.session_id)
                        .or_default()
                        .insert(request.key);
                    self.wake.wait_for(&mut state, WAIT_SLICE);
                }
            }
        }
    }
}

fn try_grant(
    state: &mut LockTable,
    session_id: u64,
    key: RowLockKey,
    strength: LockStrength,
    mark: u32,
) -> bool {
    let grants = state.rows.entry(key).or_default();
    if grants.iter().any(|grant| {
        grant.session_id != session_id && lock_strengths_conflict(grant.strength, strength)
    }) {
        return false;
    }
    if let Some(existing) = grants
        .iter_mut()
        .find(|grant| grant.session_id == session_id)
    {
        if strength > existing.strength {
            existing.strength = strength;
        }
        existing.mark = existing.mark.min(mark);
        return true;
    }
    grants.push(LockGrant {
        session_id,
        strength,
        mark,
    });
    true
}

impl RowLockManager {
    pub(crate) fn release_mark_above(&self, session_id: u64, mark: u32) {
        let mut state = self.state.lock();
        retain_session_grants(&mut state, session_id, |grant| grant.mark <= mark);
        state.waiting.remove(&session_id);
        self.wake.notify_all();
    }

    pub(crate) fn release_session(&self, session_id: u64) {
        let mut state = self.state.lock();
        retain_session_grants(&mut state, session_id, |_| false);
        state.waiting.remove(&session_id);
        self.wake.notify_all();
    }

    pub(crate) fn release_row_if_acquired(&self, session_id: u64, key: RowLockKey, mark: u32) {
        let mut state = self.state.lock();
        let Some(grants) = state.rows.get_mut(&key) else {
            return;
        };
        grants.retain(|grant| !(grant.session_id == session_id && grant.mark == mark));
        if grants.is_empty() {
            state.rows.remove(&key);
        }
        self.wake.notify_all();
    }
}

fn retain_session_grants(
    state: &mut LockTable,
    session_id: u64,
    keep: impl Fn(&LockGrant) -> bool,
) {
    state.rows.retain(|_, grants| {
        grants.retain(|grant| grant.session_id != session_id || keep(grant));
        !grants.is_empty()
    });
}

fn lock_strengths_conflict(left: LockStrength, right: LockStrength) -> bool {
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

fn deadlock_exists(state: &LockTable, waiter: u64, wanted: RowLockKey) -> bool {
    let mut stack = holders_of(state, wanted, waiter);
    let mut seen = HashSet::from([waiter]);
    while let Some(session) = stack.pop() {
        if !seen.insert(session) {
            continue;
        }
        let Some(waiting_for) = state.waiting.get(&session) else {
            continue;
        };
        for key in waiting_for {
            for holder in holders_of(state, *key, session) {
                if holder == waiter {
                    return true;
                }
                stack.push(holder);
            }
        }
    }
    false
}

fn holders_of(state: &LockTable, key: RowLockKey, except: u64) -> Vec<u64> {
    state
        .rows
        .get(&key)
        .into_iter()
        .flatten()
        .filter(|grant| grant.session_id != except)
        .map(|grant| grant.session_id)
        .collect()
}

pub(crate) fn lock_unavailable(relation: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "55P03".into(),
        message: format!("could not obtain lock on row in relation \"{relation}\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgresql_tuple_lock_conflicts_match_strength_matrix() {
        use LockStrength::{ForKeyShare, ForNoKeyUpdate, ForShare, ForUpdate};
        assert!(!lock_strengths_conflict(ForKeyShare, ForKeyShare));
        assert!(!lock_strengths_conflict(ForKeyShare, ForShare));
        assert!(!lock_strengths_conflict(ForKeyShare, ForNoKeyUpdate));
        assert!(lock_strengths_conflict(ForKeyShare, ForUpdate));
        assert!(!lock_strengths_conflict(ForShare, ForShare));
        assert!(lock_strengths_conflict(ForShare, ForNoKeyUpdate));
        assert!(lock_strengths_conflict(ForShare, ForUpdate));
        assert!(lock_strengths_conflict(ForNoKeyUpdate, ForNoKeyUpdate));
        assert!(lock_strengths_conflict(ForNoKeyUpdate, ForUpdate));
        assert!(lock_strengths_conflict(ForUpdate, ForUpdate));
    }

    #[test]
    fn acquire_reports_40p01_on_a_wait_for_cycle() {
        let manager = RowLockManager::new();
        let left = manager.allocate_session();
        let right = manager.allocate_session();
        let row_one = RowLockKey {
            table: 1,
            doc_id: 1,
        };
        let row_two = RowLockKey {
            table: 1,
            doc_id: 2,
        };
        let cancel = uqa_core::CancellationToken::new();
        let grant = |session_id, key| {
            manager.acquire(&LockRequest {
                session_id,
                key,
                strength: LockStrength::ForUpdate,
                mark: 0,
                wait: uqa_sql::ast::LockWait::Block,
                cancel: &cancel,
                relation: "accounts",
            })
        };
        assert_eq!(grant(left, row_one).unwrap(), LockAcquire::Granted);
        assert_eq!(grant(right, row_two).unwrap(), LockAcquire::Granted);

        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                manager.acquire(&LockRequest {
                    session_id: left,
                    key: row_two,
                    strength: LockStrength::ForUpdate,
                    mark: 0,
                    wait: uqa_sql::ast::LockWait::Block,
                    cancel: &cancel,
                    relation: "accounts",
                })
            });
            std::thread::sleep(Duration::from_millis(80));
            let error = grant(right, row_one).unwrap_err();
            assert_eq!(error.sqlstate(), Some("40P01"));
            manager.release_session(right);
            assert_eq!(waiter.join().unwrap().unwrap(), LockAcquire::Granted);
        });
        manager.release_session(left);
    }
}
