//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process-wide logical row locks for `FOR UPDATE` / `FOR SHARE`.
//!
//! Locks follow `PostgreSQL` 18 tuple-lock conflict rules and are held until the owning session's transaction ends or a savepoint rolls back the acquisition. Sessions inside one process arbitrate through the in-memory lock table; engines in separate OS processes over the same durable database additionally coordinate through native byte-range locks on a sidecar file next to the database.

mod change_gate;
mod change_resolution;
mod changes;
mod cleanup;
mod cross_process;
mod grants;
mod identity;
mod physical_changes;
mod registry;
mod relation;
mod waits;

use change_resolution::{
    epoch_is_after, mutation_strength, normalize_pending_row_changes, remove_inactive_versions,
    resolve_local_change_target, row_has_waiter,
};
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

#[cfg(test)]
use change_gate::change_gate_timeout;
use changes::{CommittedRowChange, CommittedRowChangeKind};
pub(crate) use changes::{
    PendingRowChange, PendingRowChangeKind, RowChangeBaseline, RowChangeObservation,
    RowChangeTarget,
};
#[cfg(test)]
use grants::MarkedStrength;
pub(crate) use grants::{lock_strengths_conflict, LockAcquire, LockRequest, RowLockAcquisition};
use grants::{rollback_grant, try_grant, GrantAttempt, LockGrant, LockTable};
pub(crate) use identity::RowLockKey;
use identity::{LockRelationIdentity, ManagerIdentity};
pub(crate) use registry::{shared_backend_manager, shared_provider_manager};
pub(crate) use relation::RelationLockMode;
use relation::{relation_modes_conflict, RelationLockGrant};
#[cfg(test)]
use waits::deadlock_exists;
use waits::{deadlock_detected, relation_deadlock_exists, CrossWaitGuard};

/// Cross-process coordination attachment for durable file databases. A sidecar that cannot be opened surfaces its reason on the first lock attempt instead of silently degrading to process-local locking.
enum CrossAttachment {
    Active(Box<FileLockCoordinator>),
    Unavailable(String),
}

pub(crate) struct RowLockManager {
    next_session: AtomicU64,
    next_transaction_xid: AtomicU64,
    relation_ids: Mutex<HashMap<LockRelationIdentity, u64>>,
    relation_identities: Mutex<HashMap<u64, LockRelationIdentity>>,
    next_table: AtomicU64,
    next_acquisition: AtomicU64,
    change_gate: RwLock<()>,
    state: Mutex<LockTable>,
    wake: Condvar,
    cross: Option<CrossAttachment>,
    column_stats: RwLock<std::collections::BTreeMap<String, crate::ColumnStatsMap>>,
}

#[cfg(test)]
mod tests;
