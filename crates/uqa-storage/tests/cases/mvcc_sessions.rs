//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session algorithms use a fault-injectable in-memory record owner, without provider dependencies.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::memory::MemoryBudget;
use uqa_storage::mvcc::*;
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{KeyValueStore, StorageBackendError};

#[test]
fn paired_handles_require_the_same_reported_transaction_context() {
    use uqa_storage::{
        KeyValueCatalog, KeyValueStorageBackend, MemoryKeyValueStore, PersistentStorageSession,
        StorageSessionMismatch,
    };

    let persistence = Persistence::new();
    let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 20));
    let b = a.open_session().unwrap();
    let legacy: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
    for (catalog, backend, expected) in [
        (a.clone(), a.clone(), true),
        (a.clone(), b, false),
        (a.clone(), legacy.clone(), false),
        (legacy.clone(), a.clone(), false),
        (legacy.clone(), legacy, true),
    ] {
        let pair = PersistentStorageSession::new(
            Arc::new(KeyValueCatalog::new(catalog)),
            Arc::new(KeyValueStorageBackend::new(backend)),
        );
        match pair.validate_transaction_affinity() {
            Ok(()) => assert!(expected),
            Err(StorageBackendError::Backend { source, .. }) => {
                assert!(!expected);
                assert!(source.downcast_ref::<StorageSessionMismatch>().is_some());
            }
            Err(error) => panic!("unexpected affinity error: {error}"),
        }
    }
    assert!(!a.in_transaction());
    assert!(a.scan_prefix(b"").unwrap().is_empty());
}

struct State {
    next: u64,
    receipts: BTreeMap<u64, CommitStatus>,
    lose_reply: bool,
}
struct Persistence {
    store: MemoryVersionStore,
    state: Mutex<State>,
}

impl Persistence {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            store: MemoryVersionStore::new(&MemoryBudget::new(1 << 24)),
            state: Mutex::new(State {
                next: 0,
                receipts: BTreeMap::new(),
                lose_reply: false,
            }),
        })
    }
    fn session(self: &Arc<Self>, retained_bytes: usize) -> VersionedKeyValueStore {
        VersionedKeyValueStore::new(
            self.clone(),
            None,
            VersionedSessionOptions { retained_bytes },
        )
    }
}

impl VersionedPersistence for Persistence {
    fn database_id(&self) -> DatabaseId {
        DatabaseId::from_bytes([9; 16])
    }
    fn allocate_transaction(&self, _: &StorageReadControl) -> VersionResult<StorageTransactionId> {
        let mut state = self.state.lock();
        state.next += 1;
        let id = StorageTransactionId::new(self.database_id(), state.next)?;
        state
            .receipts
            .insert(id.allocation(), CommitStatus::Pending);
        Ok(id)
    }
    fn snapshot(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<dyn CommittedRecordSnapshot>> {
        retain_record_snapshot(self.store.snapshot()?, control)
    }
    fn commit(
        &self,
        transaction: StorageTransactionId,
        prepared: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> CommitResult {
        let mut state = self.state.lock();
        let status = state
            .receipts
            .get(&transaction.allocation())
            .copied()
            .unwrap_or(CommitStatus::Unknown);
        if let Some(receipt) =
            resolve_prepared_receipt(status, transaction, prepared.fingerprint())?
        {
            return Ok(receipt);
        }
        let receipt = CommitReceipt {
            transaction,
            sequence: self.store.commit_prepared(prepared, control)?,
            fingerprint: prepared.fingerprint(),
        };
        state
            .receipts
            .insert(transaction.allocation(), CommitStatus::Committed(receipt));
        if state.lose_reply {
            state.lose_reply = false;
            return Err(CommitFailure::Indeterminate {
                transaction,
                source: StorageBackendError::Other("injected lost commit response".into()),
            });
        }
        Ok(receipt)
    }
    fn commit_status(
        &self,
        transaction: StorageTransactionId,
        _: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        Ok(self
            .state
            .lock()
            .receipts
            .get(&transaction.allocation())
            .copied()
            .unwrap_or(CommitStatus::Unknown))
    }
    fn abort(
        &self,
        transaction: StorageTransactionId,
        _: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        let mut state = self.state.lock();
        let entry = state
            .receipts
            .entry(transaction.allocation())
            .or_insert(CommitStatus::Unknown);
        if *entry == CommitStatus::Pending {
            *entry = CommitStatus::Aborted;
        }
        Ok(*entry)
    }
}

#[test]
fn independent_sessions_commit_while_another_retains_private_changes() {
    for ending in ["commit", "rollback", "savepoint"] {
        let persistence = Persistence::new();
        let a = persistence.session(1 << 20);
        let b = a.open_session().unwrap();
        a.begin_transaction().unwrap();
        a.put(b"a", b"first").unwrap();
        a.savepoint("s").unwrap();
        a.put(b"a", b"second").unwrap();
        b.begin_transaction().unwrap();
        b.put(b"b", b"other").unwrap();
        b.commit_transaction().unwrap();
        assert!(a.in_transaction());
        assert_eq!(a.get(b"a").unwrap().unwrap(), b"second");
        assert_eq!(a.get(b"b").unwrap(), None);
        assert_eq!(b.get(b"a").unwrap(), None);
        match ending {
            "commit" => a.commit_transaction().unwrap(),
            "rollback" => a.rollback_transaction().unwrap(),
            _ => {
                a.rollback_to_savepoint("s").unwrap();
                a.commit_transaction().unwrap();
            }
        }
        assert_eq!(b.get(b"b").unwrap().unwrap(), b"other");
        let expected = match ending {
            "commit" => Some(b"second".as_slice()),
            "savepoint" => Some(b"first".as_slice()),
            _ => None,
        };
        assert_eq!(a.get(b"a").unwrap().as_deref(), expected);
    }
}

#[test]
fn conflicts_keep_atomic_changes_private_until_rollback() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = a.open_session().unwrap();
    a.put(b"z", b"initial").unwrap();
    a.begin_transaction().unwrap();
    a.put(b"a", b"private").unwrap();
    a.put(b"z", b"stale").unwrap();
    b.put(b"z", b"winner").unwrap();
    assert!(a.commit_transaction().is_err());
    assert!(a.in_transaction());
    assert!(a.put(b"b", b"sealed").is_err());
    assert_eq!(b.get(b"a").unwrap(), None);
    a.rollback_transaction().unwrap();
    assert_eq!(a.get(b"z").unwrap().unwrap(), b"winner");
    assert_eq!(a.get(b"a").unwrap(), None);
}

#[test]
fn a_lost_commit_reply_retains_the_same_attempt_and_never_reports_rollback() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = a.open_session().unwrap();
    persistence.state.lock().lose_reply = true;
    let error = a.put(b"a", b"committed").unwrap_err();
    assert!(
        matches!(error, StorageBackendError::Backend { source, .. } if source.downcast_ref::<CommitFailure>().is_some())
    );
    let id = a.pending_commit().unwrap();
    assert_eq!(b.get(b"a").unwrap().unwrap(), b"committed");
    let revision = b.change_version().unwrap();
    assert!(a.put(b"b", b"forbidden").is_err());
    assert!(a.rollback_transaction().is_err());
    assert_eq!(a.pending_commit(), Some(id));
    a.commit_transaction().unwrap();
    assert!(!a.in_transaction());
    assert_eq!(b.change_version().unwrap(), revision);
    assert_eq!(persistence.state.lock().next, 1);
    assert_eq!(b.get(b"b").unwrap(), None);
}

#[test]
fn prefix_delete_freezes_visible_keys_and_duplicate_savepoints_resolve_nearest() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    let b = a.open_session().unwrap();
    a.put(b"p/old", b"old").unwrap();
    a.begin_transaction().unwrap();
    a.savepoint("same").unwrap();
    a.put(b"keep", b"first").unwrap();
    a.savepoint("same").unwrap();
    a.put(b"keep", b"second").unwrap();
    a.rollback_to_savepoint("same").unwrap();
    assert_eq!(a.get(b"keep").unwrap().unwrap(), b"first");
    a.release_savepoint("same").unwrap();
    assert_eq!(a.delete_prefix(b"p/").unwrap(), 1);
    b.put(b"p/new", b"other").unwrap();
    a.commit_transaction().unwrap();
    assert_eq!(
        b.scan_prefix_keys_after(b"p/", None, 3).unwrap(),
        vec![b"p/new".to_vec()]
    );
}

#[test]
fn borrowed_private_and_committed_reads_use_no_query_payload_allowance() {
    let persistence = Persistence::new();
    let a = persistence.session(1 << 20);
    a.put(b"a", b"committed").unwrap();
    a.put(b"c", b"delete").unwrap();
    a.begin_transaction().unwrap();
    a.put(b"b", b"private").unwrap();
    a.delete(b"c").unwrap();
    a.put(b"d", b"tail").unwrap();
    let control = StorageReadControl::with_limit(0);
    let mut keys = Vec::new();
    a.visit_prefix_after(b"", None, 3, &control, &mut |key, _| {
        keys.push(key.to_vec());
        Ok(())
    })
    .unwrap();
    assert_eq!(keys, vec![b"a", b"b", b"d"]);
    for key in [b"a", b"b"] {
        a.visit_value(key, &control, &mut |value| {
            assert!(value.is_some());
            Ok(())
        })
        .unwrap();
    }
    let mut calls = 0;
    let error = a
        .visit_prefix_after(b"", None, 3, &control, &mut |_, _| {
            calls += 1;
            control.cancellation().cancel();
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(error, StorageBackendError::Cancelled(_)));
    assert_eq!(calls, 1);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn batch_allocation_failure_restores_only_its_private_changes() {
    let persistence = Persistence::new();
    let mut failures = 0;
    let mut successes = 0;
    for retained_bytes in (8192..32768).step_by(512) {
        let a = persistence.session(retained_bytes);
        a.begin_transaction().unwrap();
        a.put(b"prior", b"kept").unwrap();
        let mut batch = a.batch();
        batch.put(b"small", b"new").unwrap();
        batch.put(b"large", &[7; 4096]).unwrap();
        match batch.commit() {
            Ok(()) => {
                successes += 1;
                assert_eq!(a.get(b"small").unwrap().unwrap(), b"new");
            }
            Err(StorageBackendError::Memory(_)) => {
                failures += 1;
                assert_eq!(a.get(b"small").unwrap(), None);
                assert_eq!(a.get(b"large").unwrap(), None);
            }
            Err(error) => panic!("unexpected failure: {error}"),
        }
        assert_eq!(a.get(b"prior").unwrap().unwrap(), b"kept");
        a.rollback_transaction().unwrap();
    }
    assert!(failures > 0 && successes > 0);
}
