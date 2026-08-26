//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process-wide logical row locks for `FOR UPDATE` / `FOR SHARE`.
//!
//! Locks follow `PostgreSQL` 18 tuple-lock conflict rules and are held until the owning session's transaction ends or a savepoint rolls back the acquisition. Sessions inside one process arbitrate through the in-memory lock table; engines in separate OS processes over the same durable database additionally coordinate through native byte-range locks on a sidecar file next to the database.

mod cross_process;
mod physical_changes;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use uqa_core::DocId;
use uqa_sql::ast::LockStrength;
use uqa_sql::SQLError;

use cross_process::{
    change_gate_claim, relation_byte_claims, row_byte_claims, table_hash, ByteClaim,
    FileLockCoordinator,
};
pub(crate) use physical_changes::PhysicalRowChangeTarget;
use physical_changes::{resolve_local_physical_change_target, LocalPhysicalRowChangeTarget};

const WAIT_SLICE: Duration = Duration::from_millis(50);
const CHANGE_GATE_WAIT_LIMIT: Duration = Duration::from_secs(30);
const CHANGE_GATE_SESSION: u64 = u64::MAX;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ManagerIdentity {
    Durable(uqa_storage::PersistentStorageIdentity),
    Provider(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum LockRelationIdentity {
    Table(Arc<str>),
    BackendWriter,
    KeyReservation([u8; 32]),
}

impl LockRelationIdentity {
    fn stable_bytes(&self) -> Vec<u8> {
        match self {
            Self::Table(name) => name.as_bytes().to_vec(),
            Self::BackendWriter => b"\xffbackend-writer".to_vec(),
            Self::KeyReservation(digest) => {
                let mut bytes = Vec::with_capacity(1 + "key-reservation".len() + digest.len());
                bytes.extend_from_slice(b"\xffkey-reservation");
                bytes.extend_from_slice(digest);
                bytes
            }
        }
    }
}

static DATABASE_MANAGERS: OnceLock<Mutex<HashMap<ManagerIdentity, Weak<RowLockManager>>>> =
    OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RowLockKey {
    pub table: u64,
    pub doc_id: DocId,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowChangeBaseline {
    pub(crate) epoch: u64,
    pub(crate) cross_sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PendingRowChange {
    pub(crate) key: RowLockKey,
    pub(crate) kind: PendingRowChangeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingRowChangeKind {
    Insert,
    Update,
    Delete,
    Rewrite(RowLockKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowChangeTarget {
    Unchanged,
    Present(DocId),
    Deleted,
}

#[derive(Clone, Debug)]
struct LockGrant {
    session_id: u64,
    acquisitions: Vec<MarkedStrength>,
}

#[derive(Clone, Copy, Debug)]
struct MarkedStrength {
    acquisition_id: u64,
    strength: LockStrength,
    mark: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RelationLockMode {
    RowShare,
    RowExclusive,
    AccessExclusive,
}

#[derive(Clone, Copy, Debug)]
struct MarkedRelationMode {
    mode: RelationLockMode,
    mark: u32,
}

#[derive(Clone, Debug)]
struct RelationLockGrant {
    session_id: u64,
    acquisitions: Vec<MarkedRelationMode>,
}

impl RelationLockGrant {
    fn effective_mode(&self) -> RelationLockMode {
        self.acquisitions
            .iter()
            .map(|acquisition| acquisition.mode)
            .max()
            .expect("relation-lock grant must retain an acquisition")
    }
}

impl LockGrant {
    fn effective_strength(&self) -> LockStrength {
        self.acquisitions
            .iter()
            .map(|acquisition| acquisition.strength)
            .max()
            .expect("row-lock grant must retain an acquisition")
    }
}

struct LockTable {
    rows: HashMap<RowLockKey, Vec<LockGrant>>,
    waiting: HashMap<u64, HashMap<RowLockKey, LockStrength>>,
    relations: HashMap<u64, Vec<RelationLockGrant>>,
    waiting_relations: HashMap<u64, HashMap<u64, RelationLockMode>>,
    /// The sidecar byte each local session is currently blocked on, whether its immediate holder is local or foreign. Publishing local edges lets another process walk mixed local/cross-process deadlock cycles.
    advertised_waits: HashMap<u64, ByteClaim>,
    changes: Vec<CommittedRowChange>,
    change_epoch: u64,
    active_change_observers: usize,
}

#[derive(Clone, Copy, Debug)]
struct CommittedRowChange {
    epoch: u64,
    key: RowLockKey,
    kind: CommittedRowChangeKind,
    strength: LockStrength,
}

#[derive(Clone, Copy, Debug)]
enum CommittedRowChangeKind {
    Update,
    Delete,
    Rewrite(RowLockKey),
}

/// Cross-process coordination attachment for durable file databases. A sidecar that cannot be opened surfaces its reason on the first lock attempt instead of silently degrading to process-local locking.
enum CrossAttachment {
    Active(FileLockCoordinator),
    Unavailable(String),
}

pub(crate) struct RowLockManager {
    next_session: AtomicU64,
    relation_ids: Mutex<HashMap<LockRelationIdentity, u64>>,
    relation_identities: Mutex<HashMap<u64, LockRelationIdentity>>,
    next_table: AtomicU64,
    next_acquisition: AtomicU64,
    change_gate: RwLock<()>,
    state: Mutex<LockTable>,
    wake: Condvar,
    cross: Option<CrossAttachment>,
}

pub(crate) struct RowChangeSnapshot<'manager> {
    manager: &'manager RowLockManager,
    cross_claim: Option<ByteClaim>,
    _local: RwLockReadGuard<'manager, ()>,
}

pub(crate) struct RowChangePublication<'manager> {
    manager: &'manager RowLockManager,
    cross_claim: Option<ByteClaim>,
    _local: RwLockWriteGuard<'manager, ()>,
}

impl RowChangeSnapshot<'_> {
    pub(crate) fn baseline(&self) -> Result<RowChangeBaseline, SQLError> {
        let epoch = self.manager.current_change_epoch();
        let cross_sequence = match self.manager.cross.as_ref() {
            Some(CrossAttachment::Active(coordinator)) => {
                coordinator.change_sequence().map_err(SQLError::Internal)?
            }
            _ => 0,
        };
        Ok(RowChangeBaseline {
            epoch,
            cross_sequence,
        })
    }
}

impl Drop for RowChangeSnapshot<'_> {
    fn drop(&mut self) {
        if let (Some(CrossAttachment::Active(coordinator)), Some(claim)) =
            (self.manager.cross.as_ref(), self.cross_claim)
        {
            coordinator.release(CHANGE_GATE_SESSION, &[claim]);
        }
    }
}

impl Drop for RowChangePublication<'_> {
    fn drop(&mut self) {
        if let (Some(CrossAttachment::Active(coordinator)), Some(claim)) =
            (self.manager.cross.as_ref(), self.cross_claim)
        {
            coordinator.release(CHANGE_GATE_SESSION, &[claim]);
        }
    }
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
    session_id: u64,
    key: RowLockKey,
    acquisition_id: u64,
}

pub(crate) struct RowChangeObservation {
    manager: Arc<RowLockManager>,
}

impl Drop for RowChangeObservation {
    fn drop(&mut self) {
        self.manager.end_change_observation();
    }
}

enum GrantAttempt {
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

impl RowLockManager {
    pub(crate) fn new() -> Self {
        Self::with_cross_attachment(None)
    }

    fn with_cross_attachment(cross: Option<CrossAttachment>) -> Self {
        Self {
            next_session: AtomicU64::new(1),
            relation_ids: Mutex::new(HashMap::new()),
            relation_identities: Mutex::new(HashMap::new()),
            next_table: AtomicU64::new(1),
            next_acquisition: AtomicU64::new(1),
            change_gate: RwLock::new(()),
            state: Mutex::new(LockTable {
                rows: HashMap::new(),
                waiting: HashMap::new(),
                relations: HashMap::new(),
                waiting_relations: HashMap::new(),
                advertised_waits: HashMap::new(),
                changes: Vec::new(),
                change_epoch: 0,
                active_change_observers: 0,
            }),
            wake: Condvar::new(),
            cross,
        }
    }

    fn for_database_file(path: &std::path::Path) -> Self {
        #[cfg(any(unix, windows))]
        {
            let cross = match FileLockCoordinator::open(path) {
                Ok(coordinator) => CrossAttachment::Active(coordinator),
                Err(reason) => CrossAttachment::Unavailable(reason),
            };
            Self::with_cross_attachment(Some(cross))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Self::with_cross_attachment(None)
        }
    }

    pub(crate) fn allocate_session(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn table_key(&self, table: &str) -> u64 {
        self.relation_key(LockRelationIdentity::Table(Arc::from(table)))
    }

    pub(crate) fn backend_writer_key(&self) -> u64 {
        self.relation_key(LockRelationIdentity::BackendWriter)
    }

    pub(crate) fn key_reservation_key(&self, digest: [u8; 32]) -> u64 {
        self.relation_key(LockRelationIdentity::KeyReservation(digest))
    }

    fn relation_key(&self, identity: LockRelationIdentity) -> u64 {
        let mut relations = self.relation_ids.lock();
        if let Some(id) = relations.get(&identity) {
            return *id;
        }
        let id = self.next_table.fetch_add(1, Ordering::Relaxed);
        relations.insert(identity.clone(), id);
        self.relation_identities.lock().insert(id, identity);
        id
    }

    pub(crate) fn stable_table_hash(table: &str) -> u64 {
        table_hash(table.as_bytes())
    }

    pub(crate) fn table_name(&self, table: u64) -> Arc<str> {
        match self.relation_identities.lock().get(&table).cloned() {
            Some(LockRelationIdentity::Table(name)) => name,
            Some(_) => panic!("internal lock identity has no SQL table name"),
            None => panic!("unknown relation lock identity"),
        }
    }

    fn relation_bytes(&self, table: u64) -> Vec<u8> {
        self.relation_identities
            .lock()
            .get(&table)
            .unwrap_or_else(|| panic!("unknown relation lock identity"))
            .stable_bytes()
    }

    fn coordinator(&self) -> Result<Option<&FileLockCoordinator>, SQLError> {
        match self.cross.as_ref() {
            None => Ok(None),
            Some(CrossAttachment::Active(coordinator)) => Ok(Some(coordinator)),
            Some(CrossAttachment::Unavailable(reason)) => Err(SQLError::Internal(format!(
                "cross-process lock coordination is unavailable: {reason}"
            ))),
        }
    }

    /// Whether this manager has durable cross-process coordination. Unlike a peer-liveness check, this remains true after a peer exits, because commits made by that peer can still be newer than a statement snapshot.
    pub(crate) fn has_cross_process_coordination(&self) -> bool {
        matches!(self.cross.as_ref(), Some(CrossAttachment::Active(_)))
    }

    fn acquire_change_gate_claim(
        &self,
        write: bool,
        cancel: &uqa_core::CancellationToken,
        deadline: Instant,
    ) -> Result<Option<ByteClaim>, SQLError> {
        let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() else {
            return Ok(None);
        };
        let claim = change_gate_claim(write);
        loop {
            cancel.check()?;
            if let Ok(()) = coordinator
                .try_claim(CHANGE_GATE_SESSION, &[claim])
                .map_err(SQLError::Internal)?
            {
                return Ok(Some(claim));
            }
            if Instant::now() >= deadline {
                return Err(change_gate_timeout());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Hold the shared commit/snapshot gate while a storage snapshot is pinned and its row-change baseline is captured.
    pub(crate) fn begin_change_snapshot(
        &self,
        cancel: &uqa_core::CancellationToken,
    ) -> Result<RowChangeSnapshot<'_>, SQLError> {
        let deadline = Instant::now() + CHANGE_GATE_WAIT_LIMIT;
        let local = loop {
            cancel.check()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(change_gate_timeout());
            }
            if let Some(local) = self.change_gate.try_read_for(remaining.min(WAIT_SLICE)) {
                break local;
            }
        };
        let cross_claim = self.acquire_change_gate_claim(false, cancel, deadline)?;
        Ok(RowChangeSnapshot {
            manager: self,
            cross_claim,
            _local: local,
        })
    }

    /// Hold the exclusive commit/snapshot gate from immediately before the backend commit through publication of its row-change metadata.
    pub(crate) fn begin_change_publication(
        &self,
        cancel: &uqa_core::CancellationToken,
    ) -> Result<RowChangePublication<'_>, SQLError> {
        let deadline = Instant::now() + CHANGE_GATE_WAIT_LIMIT;
        let local = loop {
            cancel.check()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(change_gate_timeout());
            }
            if let Some(local) = self.change_gate.try_write_for(remaining.min(WAIT_SLICE)) {
                break local;
            }
        };
        let cross_claim = self.acquire_change_gate_claim(true, cancel, deadline)?;
        Ok(RowChangePublication {
            manager: self,
            cross_claim,
            _local: local,
        })
    }

    fn release_row_claims(&self, session_id: u64, key: RowLockKey, strength: LockStrength) {
        if let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() {
            let relation = self.relation_bytes(key.table);
            coordinator.release(
                session_id,
                &row_byte_claims(&relation, key.doc_id, strength),
            );
        }
    }

    fn release_relation_claims(&self, session_id: u64, table: u64, mode: RelationLockMode) {
        if let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() {
            let relation = self.relation_bytes(table);
            coordinator.release(session_id, &relation_byte_claims(&relation, mode));
        }
    }

    /// Whether the cross-process wait-for graph from `wanted` closes back on `session_id`. Bytes held by other local sessions are followed through those sessions' own foreign waits.
    fn cross_wait_cycle(
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

fn deadlock_detected() -> SQLError {
    SQLError::Routine {
        sqlstate: "40P01".into(),
        message: "deadlock detected".into(),
    }
}

fn change_gate_timeout() -> SQLError {
    SQLError::Routine {
        sqlstate: "55P03".into(),
        message: format!(
            "timed out after {} seconds waiting for cross-process row-change coordination",
            CHANGE_GATE_WAIT_LIMIT.as_secs()
        ),
    }
}

/// Advertises one session's cross-process wait for the duration of an acquisition and clears it on every exit path.
struct CrossWaitGuard<'a> {
    manager: &'a RowLockManager,
    coordinator: Option<&'a FileLockCoordinator>,
    session_id: u64,
    registered: std::cell::Cell<bool>,
}

impl<'a> CrossWaitGuard<'a> {
    fn new(
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

    fn register(&self, state: &mut LockTable, claim: ByteClaim) {
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

/// Undo one just-granted row acquisition whose cross-process claim failed.
fn rollback_grant(state: &mut LockTable, acquisition: RowLockAcquisition) {
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

pub(crate) fn shared_provider_manager(
    identity: Option<uqa_storage::PersistentStorageIdentity>,
    provider: &Arc<dyn uqa_storage::PersistentStorageProvider>,
) -> Arc<RowLockManager> {
    shared_manager(
        identity,
        ManagerIdentity::Provider(Arc::as_ptr(provider).cast::<()>() as usize),
    )
}

pub(crate) fn shared_backend_manager(
    identity: Option<uqa_storage::PersistentStorageIdentity>,
    backend: &Arc<dyn uqa_storage::PersistentStorageBackend>,
) -> Arc<RowLockManager> {
    shared_manager(
        identity,
        ManagerIdentity::Provider(Arc::as_ptr(backend).cast::<()>() as usize),
    )
}

fn shared_manager(
    identity: Option<uqa_storage::PersistentStorageIdentity>,
    fallback: ManagerIdentity,
) -> Arc<RowLockManager> {
    let identity = identity.map_or(fallback, ManagerIdentity::Durable);
    let registry = DATABASE_MANAGERS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry.lock();
    registry.retain(|_, manager| manager.strong_count() != 0);
    if let Some(manager) = registry.get(&identity).and_then(Weak::upgrade) {
        return manager;
    }
    let manager = match &identity {
        ManagerIdentity::Durable(uqa_storage::PersistentStorageIdentity::File(path)) => {
            Arc::new(RowLockManager::for_database_file(path))
        }
        _ => Arc::new(RowLockManager::new()),
    };
    registry.insert(identity, Arc::downgrade(&manager));
    manager
}

fn try_grant(
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

impl RowLockManager {
    pub(crate) fn begin_change_observation(self: &Arc<Self>) -> RowChangeObservation {
        self.state.lock().active_change_observers += 1;
        RowChangeObservation {
            manager: Arc::clone(self),
        }
    }

    fn end_change_observation(&self) {
        let mut state = self.state.lock();
        state.active_change_observers = state.active_change_observers.saturating_sub(1);
        remove_inactive_versions(&mut state);
    }

    pub(crate) fn current_change_epoch(&self) -> u64 {
        self.state.lock().change_epoch
    }

    pub(crate) fn release_mark_above(&self, session_id: u64, mark: u32) {
        let mut released_rows = Vec::new();
        let mut released_relations = Vec::new();
        let mut state = self.state.lock();
        state.rows.retain(|key, grants| {
            grants.retain_mut(|grant| {
                if grant.session_id == session_id {
                    grant.acquisitions.retain(|acquisition| {
                        let keep = acquisition.mark <= mark;
                        if !keep {
                            released_rows.push((*key, acquisition.strength));
                        }
                        keep
                    });
                }
                !grant.acquisitions.is_empty()
            });
            !grants.is_empty()
        });
        state.relations.retain(|table, grants| {
            grants.retain_mut(|grant| {
                if grant.session_id == session_id {
                    grant.acquisitions.retain(|acquisition| {
                        let keep = acquisition.mark <= mark;
                        if !keep {
                            released_relations.push((*table, acquisition.mode));
                        }
                        keep
                    });
                }
                !grant.acquisitions.is_empty()
            });
            !grants.is_empty()
        });
        state.waiting.remove(&session_id);
        state.waiting_relations.remove(&session_id);
        state.advertised_waits.remove(&session_id);
        remove_inactive_versions(&mut state);
        drop(state);
        for (key, strength) in released_rows {
            self.release_row_claims(session_id, key, strength);
        }
        for (table, mode) in released_relations {
            self.release_relation_claims(session_id, table, mode);
        }
        self.wake.notify_all();
    }

    pub(crate) fn release_session(&self, session_id: u64) {
        let mut released_rows = Vec::new();
        let mut released_relations = Vec::new();
        let mut state = self.state.lock();
        state.rows.retain(|key, grants| {
            grants.retain(|grant| {
                if grant.session_id == session_id {
                    for acquisition in &grant.acquisitions {
                        released_rows.push((*key, acquisition.strength));
                    }
                    return false;
                }
                true
            });
            !grants.is_empty()
        });
        state.relations.retain(|table, grants| {
            grants.retain(|grant| {
                if grant.session_id == session_id {
                    for acquisition in &grant.acquisitions {
                        released_relations.push((*table, acquisition.mode));
                    }
                    return false;
                }
                true
            });
            !grants.is_empty()
        });
        state.waiting.remove(&session_id);
        state.waiting_relations.remove(&session_id);
        state.advertised_waits.remove(&session_id);
        remove_inactive_versions(&mut state);
        drop(state);
        for (key, strength) in released_rows {
            self.release_row_claims(session_id, key, strength);
        }
        for (table, mode) in released_relations {
            self.release_relation_claims(session_id, table, mode);
        }
        self.wake.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn current_row_version(&self, table: &str, doc_id: DocId) -> u64 {
        let key = RowLockKey {
            table: self.table_key(table),
            doc_id,
        };
        self.state
            .lock()
            .changes
            .iter()
            .rev()
            .find(|change| change.key == key)
            .map_or(0, |change| change.epoch)
    }

    pub(crate) fn conflicting_change_target_after(
        &self,
        table: &str,
        doc_id: DocId,
        baseline: RowChangeBaseline,
        wanted: LockStrength,
    ) -> Result<RowChangeTarget, SQLError> {
        let key = RowLockKey {
            table: self.table_key(table),
            doc_id,
        };
        if let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() {
            return coordinator
                .change_target_after(
                    table_hash(table.as_bytes()),
                    doc_id,
                    baseline.cross_sequence,
                    wanted,
                )
                .map_err(SQLError::Internal);
        }
        Ok(resolve_local_change_target(
            &self.state.lock().changes,
            key,
            baseline.epoch,
            wanted,
        ))
    }

    /// Follow committed primary-key rewrites from `doc_id` to the final identity, considering only rewrites newer than the statement snapshot. This prevents an old update chain from attaching to a later row that reused the same primary key.
    pub(crate) fn row_successor_after(
        &self,
        table: &str,
        doc_id: DocId,
        baseline: RowChangeBaseline,
    ) -> Result<RowChangeTarget, SQLError> {
        let key = RowLockKey {
            table: self.table_key(table),
            doc_id,
        };
        if let Some(CrossAttachment::Active(coordinator)) = self.cross.as_ref() {
            return coordinator
                .change_target_after(
                    table_hash(table.as_bytes()),
                    doc_id,
                    baseline.cross_sequence,
                    LockStrength::ForUpdate,
                )
                .map_err(SQLError::Internal);
        }
        Ok(resolve_local_change_target(
            &self.state.lock().changes,
            key,
            baseline.epoch,
            LockStrength::ForUpdate,
        ))
    }

    pub(crate) fn publish_row_changes(
        &self,
        session_id: u64,
        changes: impl IntoIterator<Item = PendingRowChange>,
    ) -> Result<(), SQLError> {
        let changes = normalize_pending_row_changes(changes);
        if changes.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock();
        state.change_epoch = state.change_epoch.wrapping_add(1);
        let change_epoch = state.change_epoch;
        let committed = changes
            .into_iter()
            .filter_map(|change| {
                let kind = match change.kind {
                    PendingRowChangeKind::Insert => return None,
                    PendingRowChangeKind::Update => CommittedRowChangeKind::Update,
                    PendingRowChangeKind::Delete => CommittedRowChangeKind::Delete,
                    PendingRowChangeKind::Rewrite(successor) => {
                        CommittedRowChangeKind::Rewrite(successor)
                    }
                };
                let strength = match kind {
                    CommittedRowChangeKind::Rewrite(_) | CommittedRowChangeKind::Delete => {
                        LockStrength::ForUpdate
                    }
                    CommittedRowChangeKind::Update => {
                        mutation_strength(&state, session_id, change.key)
                    }
                };
                Some(CommittedRowChange {
                    epoch: change_epoch,
                    key: change.key,
                    kind,
                    strength,
                })
            })
            .collect::<Vec<_>>();
        let publication_result = if let Some(CrossAttachment::Active(coordinator)) =
            self.cross.as_ref()
        {
            let published = committed
                .iter()
                .map(|change| cross_process::PublishedRowChange {
                    table_hash: table_hash(&self.relation_bytes(change.key.table)),
                    doc_id: change.key.doc_id,
                    kind: match change.kind {
                        CommittedRowChangeKind::Update => {
                            cross_process::PublishedRowChangeKind::Update
                        }
                        CommittedRowChangeKind::Delete => {
                            cross_process::PublishedRowChangeKind::Delete
                        }
                        CommittedRowChangeKind::Rewrite(successor) => {
                            cross_process::PublishedRowChangeKind::Rewrite(
                                cross_process::PublishedRowIdentity {
                                    table_hash: table_hash(&self.relation_bytes(successor.table)),
                                    doc_id: successor.doc_id,
                                },
                            )
                        }
                    },
                    strength: change.strength,
                })
                .collect::<Vec<_>>();
            coordinator
                .publish_changes(&published)
                .map_err(SQLError::Internal)
        } else {
            Ok(())
        };
        for change in committed {
            let key = change.key;
            if state.active_change_observers != 0
                || state.rows.contains_key(&key)
                || row_has_waiter(&state, key)
            {
                state.changes.push(change);
            }
        }
        publication_result
    }

    pub(crate) fn rollback_acquisition(&self, acquisition: RowLockAcquisition) {
        let mut released = None;
        let mut state = self.state.lock();
        let Some(grants) = state.rows.get_mut(&acquisition.key) else {
            return;
        };
        grants.retain_mut(|grant| {
            if grant.session_id == acquisition.session_id {
                grant.acquisitions.retain(|marked| {
                    let keep = marked.acquisition_id != acquisition.acquisition_id;
                    if !keep {
                        released = Some(marked.strength);
                    }
                    keep
                });
            }
            !grant.acquisitions.is_empty()
        });
        if grants.is_empty() {
            state.rows.remove(&acquisition.key);
        }
        remove_inactive_versions(&mut state);
        drop(state);
        if let Some(strength) = released {
            self.release_row_claims(acquisition.session_id, acquisition.key, strength);
        }
        self.wake.notify_all();
    }
}

fn normalize_pending_row_changes(
    changes: impl IntoIterator<Item = PendingRowChange>,
) -> Vec<PendingRowChange> {
    let changes = changes.into_iter().collect::<Vec<_>>();
    let mut skip = vec![false; changes.len()];
    for (rewrite_index, rewrite) in changes.iter().enumerate() {
        let PendingRowChangeKind::Rewrite(successor) = rewrite.kind else {
            continue;
        };
        let Some(delete_index) = (0..rewrite_index).rev().find(|index| {
            !skip[*index]
                && changes[*index].key == rewrite.key
                && matches!(changes[*index].kind, PendingRowChangeKind::Delete)
        }) else {
            continue;
        };
        let Some(insert_index) = (delete_index + 1..rewrite_index).rev().find(|index| {
            !skip[*index]
                && changes[*index].key == successor
                && matches!(changes[*index].kind, PendingRowChangeKind::Insert)
        }) else {
            continue;
        };
        skip[delete_index] = true;
        skip[insert_index] = true;
        for index in insert_index + 1..rewrite_index {
            if changes[index].key == successor
                && matches!(changes[index].kind, PendingRowChangeKind::Update)
            {
                skip[index] = true;
            }
        }
    }

    let mut created = HashSet::new();
    let mut normalized = Vec::new();
    for (index, change) in changes.into_iter().enumerate() {
        if skip[index] {
            continue;
        }
        match change.kind {
            PendingRowChangeKind::Insert => {
                created.insert(change.key);
            }
            PendingRowChangeKind::Update if created.contains(&change.key) => {}
            PendingRowChangeKind::Delete if created.remove(&change.key) => {}
            PendingRowChangeKind::Rewrite(successor) if created.remove(&change.key) => {
                created.insert(successor);
            }
            PendingRowChangeKind::Update
            | PendingRowChangeKind::Delete
            | PendingRowChangeKind::Rewrite(_) => normalized.push(change),
        }
    }
    normalized
}

fn resolve_local_change_target(
    changes: &[CommittedRowChange],
    key: RowLockKey,
    baseline: u64,
    wanted: LockStrength,
) -> RowChangeTarget {
    match resolve_local_physical_change_target(changes, key, baseline, wanted) {
        LocalPhysicalRowChangeTarget::Unchanged => RowChangeTarget::Unchanged,
        LocalPhysicalRowChangeTarget::Present(target) if target.table == key.table => {
            RowChangeTarget::Present(target.doc_id)
        }
        // Callers of the legacy document-id-only API cannot safely follow a tuple into another physical relation. Treat it as absent instead of applying the successor id to an unrelated row in the source relation.
        LocalPhysicalRowChangeTarget::Present(_) | LocalPhysicalRowChangeTarget::Deleted => {
            RowChangeTarget::Deleted
        }
    }
}

fn epoch_is_after(candidate: u64, baseline: u64) -> bool {
    let distance = candidate.wrapping_sub(baseline);
    distance != 0 && distance <= u64::MAX / 2
}

fn mutation_strength(state: &LockTable, session_id: u64, key: RowLockKey) -> LockStrength {
    state
        .rows
        .get(&key)
        .and_then(|grants| grants.iter().find(|grant| grant.session_id == session_id))
        .map(LockGrant::effective_strength)
        .filter(|strength| {
            matches!(
                strength,
                LockStrength::ForNoKeyUpdate | LockStrength::ForUpdate
            )
        })
        .unwrap_or(LockStrength::ForUpdate)
}

fn row_has_waiter(state: &LockTable, key: RowLockKey) -> bool {
    state
        .waiting
        .values()
        .any(|requests| requests.contains_key(&key))
}

fn remove_inactive_versions(state: &mut LockTable) {
    if state.active_change_observers != 0 {
        return;
    }
    let rows = &state.rows;
    let waiting = &state.waiting;
    state.changes.retain(|change| {
        rows.contains_key(&change.key)
            || waiting
                .values()
                .any(|requests| requests.contains_key(&change.key))
    });
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

fn deadlock_exists(
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

fn relation_deadlock_exists(
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

fn relation_modes_conflict(left: RelationLockMode, right: RelationLockMode) -> bool {
    left == RelationLockMode::AccessExclusive || right == RelationLockMode::AccessExclusive
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

#[cfg(test)]
mod tests;
