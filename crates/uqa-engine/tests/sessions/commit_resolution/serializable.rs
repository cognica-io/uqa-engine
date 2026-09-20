//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine completion adapters retain logical SSI outcomes without manufacturing physical writes.

use super::*;
use uqa_storage::mvcc::{SerializableCoordinator, SerializableOperation, TransactionOutcomeId};

impl SerializableCoordinator for FaultPersistence {
    fn with_serializable_admission(
        &self,
        control: &StorageReadControl,
        operation: &mut SerializableOperation<'_>,
    ) -> VersionResult<()> {
        let fault = self.serializable_fault.load(Ordering::Acquire);
        if fault == UNAVAILABLE {
            return Err(StorageBackendError::Other(
                "injected unavailable logical completion".into(),
            )
            .into());
        }
        self.inner
            .serializable_coordinator()
            .unwrap()
            .with_serializable_admission(control, operation)?;
        if fault == LOSE_COMMITTED_REPLY {
            self.serializable_fault
                .store(UNAVAILABLE, Ordering::Release);
            return Err(StorageBackendError::Other(
                "injected lost logical completion reply".into(),
            )
            .into());
        }
        Ok(())
    }
}

fn retained_engine(persistence: Arc<FaultPersistence>) -> (Engine, Arc<VersionedKeyValueStore>) {
    let store = Arc::new(VersionedKeyValueStore::new(
        persistence,
        None,
        VersionedSessionOptions::default(),
    ));
    let root = Engine::from_persistent_backends(
        Arc::new(KeyValueCatalog::new(store.clone())),
        Arc::new(KeyValueStorageBackend::new(store.clone())),
    )
    .unwrap();
    (root, store)
}

#[test]
fn a_read_only_logical_commit_keeps_the_frame_and_resolves_rollback_against_its_original_outcome() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let (root, store) = retained_engine(persistence.clone());
        root.sql("BEGIN READ ONLY", &[]).unwrap();
        // Exercise the session-to-Engine completion boundary independently of unfinished SQL predicate wiring.
        let actor = store.establish_serializable_snapshot().unwrap();
        persistence
            .serializable_fault
            .store(LOSE_COMMITTED_REPLY, Ordering::Release);
        assert_unknown(&root.commit().unwrap_err());
        assert_eq!(root.pending_commit(), None);
        assert_eq!(
            root.pending_transaction_completion(),
            Some(TransactionOutcomeId::Serializable(actor.id()))
        );
        assert_unknown(&root.sql("SELECT 1", &[]).unwrap_err());
        assert_unknown(&root.rollback().unwrap_err());
        assert_eq!(root.transaction_depth(), 1);
        persistence
            .serializable_fault
            .store(HEALTHY, Ordering::Release);
        assert_eq!(root.rollback().unwrap_err().sqlstate(), Some("25000"));
        assert_eq!(root.pending_transaction_completion(), None);
        assert_eq!(root.transaction_depth(), 0);
        assert_eq!(persistence.aborts.load(Ordering::Acquire), 0);
    }
}

#[test]
fn a_scoped_empty_commit_preserves_logical_resolution_without_replaying_the_callback() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let (root, store) = retained_engine(persistence.clone());
        let calls = AtomicUsize::new(0);
        let mut id = None;
        let error = root
            .transaction(|_| {
                calls.fetch_add(1, Ordering::AcqRel);
                id = Some(store.establish_serializable_snapshot().unwrap().id());
                persistence
                    .serializable_fault
                    .store(LOSE_COMMITTED_REPLY, Ordering::Release);
                Ok(())
            })
            .unwrap_err();
        assert_unknown(&error);
        assert_eq!(root.pending_commit(), None);
        assert_eq!(
            root.pending_transaction_completion(),
            Some(TransactionOutcomeId::Serializable(id.unwrap()))
        );
        assert_eq!(root.transaction_depth(), 1);
        assert_eq!(persistence.aborts.load(Ordering::Acquire), 0);
        persistence
            .serializable_fault
            .store(HEALTHY, Ordering::Release);
        root.commit().unwrap();
        assert_eq!(root.pending_transaction_completion(), None);
        assert_eq!(root.transaction_depth(), 0);
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }
}
