//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine completion adapters retain logical SSI outcomes without manufacturing physical writes.

use super::*;
use uqa_storage::mvcc::{
    SerializableCoordinator, SerializableOperation, SerializableSession, SerializableStatus,
    SerializableTransactionId, TransactionOutcomeId,
};

impl FaultPersistence {
    fn lose_serializable_commit_reply(&self, target: SerializableTransactionId) {
        *self.serializable_commit.lock().unwrap() = Some(target);
        self.serializable_fault
            .store(LOSE_COMMITTED_REPLY, Ordering::Release);
    }
}

impl SerializableCoordinator for FaultPersistence {
    fn with_serializable_admission(
        &self,
        control: &StorageReadControl,
        operation: &mut SerializableOperation<'_>,
    ) -> VersionResult<()> {
        let fault = if std::thread::current().id() == self.foreground {
            self.serializable_fault.load(Ordering::Acquire)
        } else {
            HEALTHY
        };
        if fault == UNAVAILABLE {
            return Err(StorageBackendError::Other(
                "injected unavailable logical completion".into(),
            )
            .into());
        }
        let coordinator = self.inner.serializable_coordinator().unwrap();
        if fault != LOSE_COMMITTED_REPLY {
            return coordinator.with_serializable_admission(control, operation);
        }
        let target = self
            .serializable_commit
            .lock()
            .unwrap()
            .expect("lost completion reply has an original participant");
        let mut committed = false;
        // Command refresh can precede COMMIT; lose only the selected participant's persisted completion.
        coordinator.with_serializable_admission(control, &mut |graph, leases| {
            operation(graph, leases)?;
            committed = graph.status(target)? == SerializableStatus::Committed;
            Ok(())
        })?;
        if committed {
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
    root.sql(
        "CREATE TABLE completion_source (id INTEGER PRIMARY KEY)",
        &[],
    )
    .unwrap();
    root.sql("INSERT INTO completion_source VALUES (1)", &[])
        .unwrap();
    (root, store)
}

#[test]
fn a_read_only_logical_commit_keeps_the_frame_and_resolves_rollback_against_its_original_outcome() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let (root, store) = retained_engine(persistence.clone());
        let record_writes = persistence.foreground_record_writes();
        root.sql("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY", &[])
            .unwrap();
        assert!(store.serializable_read_context().unwrap().is_none());
        root.sql("SELECT id FROM completion_source", &[]).unwrap();
        let actor = store
            .serializable_read_context()
            .unwrap()
            .expect("SQL admitted its original participant");
        persistence.lose_serializable_commit_reply(actor.id());
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
        assert!(store.serializable_read_context().unwrap().is_none());
        assert_eq!(persistence.foreground_record_writes(), record_writes);
    }
}

#[test]
fn a_scoped_empty_commit_preserves_logical_resolution_without_replaying_the_callback() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let (root, store) = retained_engine(persistence.clone());
        root.sql("SET default_transaction_isolation = 'serializable'", &[])
            .unwrap();
        let record_writes = persistence.foreground_record_writes();
        let calls = AtomicUsize::new(0);
        let mut id = None;
        let error = root
            .transaction(|engine| {
                calls.fetch_add(1, Ordering::AcqRel);
                assert!(store.serializable_read_context().unwrap().is_none());
                engine.sql("SELECT id FROM completion_source", &[])?;
                let actor = store
                    .serializable_read_context()
                    .unwrap()
                    .expect("SQL admitted its original participant")
                    .id();
                id = Some(actor);
                persistence.lose_serializable_commit_reply(actor);
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
        assert!(store.serializable_read_context().unwrap().is_none());
        assert_eq!(persistence.foreground_record_writes(), record_writes);
    }
}
