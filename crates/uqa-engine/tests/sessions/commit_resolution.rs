//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine receipt resolution preserves the prepared SQL effects over real provider records.

#[path = "commit_resolution/conflicts.rs"]
mod conflicts;
#[path = "commit_resolution/notifications.rs"]
mod notifications;
#[path = "commit_resolution/serializable.rs"]
mod serializable;

use std::sync::{
    atomic::{AtomicU8, AtomicUsize, Ordering},
    Arc, Mutex,
};

use uqa_core::Value;
use uqa_engine::{Engine, SQLFunctionOptions, SQLFunctionVolatility};
use uqa_sql::SQLError;
use uqa_storage::mvcc::{
    CommitFailure, CommitResult, CommitStatus, CommittedRecordSnapshot, DatabaseId,
    PreparedRecordCommit, StorageTransactionId, VersionResult, VersionedKeyValueStore,
    VersionedPersistence, VersionedSessionOptions,
};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{KeyValueCatalog, KeyValueStorageBackend, KeyValueStore, StorageBackendError};
use uqa_storage_redb::RedbStorage;
use uqa_storage_sqlite::{ManagedConnection, SQLiteCompressionOptions, SQLiteRecordStore};

const HEALTHY: u8 = 0;
const LOSE_COMMITTED_REPLY: u8 = 1;
const UNAVAILABLE: u8 = 2;
const LOSE_UNCOMMITTED_REPLY: u8 = 3;
const LOSE_ABORT_REPLY: u8 = 4;
const HIDE_AFTER_ABORT_RECEIPT: u8 = 5;
const REJECT_MEMORY: u8 = 6;
const REJECT_CANCELLED: u8 = 7;
const REJECT_CONSTRAINT: u8 = 8;
const REJECT_OTHER: u8 = 9;
const REJECT_DEPENDENCY: u8 = 10;
const LOSE_CONFLICT_REPLY: u8 = 11;
const LOSE_ABORT_REPLY_ONCE: u8 = 12;

fn rejected_commit_error(fault: u8) -> Option<uqa_storage::mvcc::VersionError> {
    use uqa_storage::mvcc::{CommitSequence, VersionError};
    Some(match fault {
        REJECT_MEMORY => uqa_core::memory::MemoryError::SizeOverflow.into(),
        REJECT_CANCELLED => uqa_core::QueryCancelled.into(),
        REJECT_CONSTRAINT => StorageBackendError::backend(
            "fixture",
            SQLError::Routine {
                sqlstate: "23505".into(),
                message: "injected unique constraint violation".into(),
            },
        )
        .into(),
        REJECT_OTHER => StorageBackendError::Other("injected storage failure".into()).into(),
        REJECT_DEPENDENCY => VersionError::ReadConflict {
            dependency: 0,
            expected: Some(CommitSequence::from_u64(1)),
            actual: Some(CommitSequence::from_u64(2)),
        },
        _ => return None,
    })
}

struct FaultPersistence {
    inner: Arc<dyn VersionedPersistence>,
    fault: AtomicU8,
    identifier_fault: AtomicU8,
    serializable_fault: AtomicU8,
    serializable_completion: Mutex<Option<uqa_storage::mvcc::SerializableTransactionId>>,
    foreground: std::thread::ThreadId,
    foreground_transaction_allocations: AtomicUsize,
    foreground_record_commits: AtomicUsize,
    attempt: Mutex<Option<StorageTransactionId>>,
    aborts: AtomicUsize,
}

impl FaultPersistence {
    fn foreground_record_writes(&self) -> (usize, usize) {
        (
            self.foreground_transaction_allocations
                .load(Ordering::Acquire),
            self.foreground_record_commits.load(Ordering::Acquire),
        )
    }

    fn fault_for(&self, transaction: StorageTransactionId) -> u8 {
        let fault = self.fault.load(Ordering::Acquire);
        if fault == HEALTHY {
            return HEALTHY;
        }
        // Only the foreground may select the target; subsequent faults follow that transaction even when another thread resolves it.
        let mut attempt = self.attempt.lock().unwrap();
        if attempt.is_none() && std::thread::current().id() != self.foreground {
            return HEALTHY;
        }
        let target = *attempt.get_or_insert(transaction);
        if target == transaction {
            fault
        } else {
            HEALTHY
        }
    }
}

impl VersionedPersistence for FaultPersistence {
    fn serializable_coordinator(&self) -> Option<&dyn uqa_storage::mvcc::SerializableCoordinator> {
        Some(self)
    }
    fn identifier_watermark(
        &self,
        namespace: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<u64>> {
        self.inner.identifier_watermark(namespace, control)
    }

    fn database_id(&self) -> DatabaseId {
        self.inner.database_id()
    }
    fn graph_record_layout(&self) -> Option<&dyn uqa_storage::mvcc::GraphRecordLayout> {
        self.inner.graph_record_layout()
    }
    fn allocate_identifiers(
        &self,
        namespace: &[u8],
        request: uqa_storage::mvcc::IdentifierRequest,
        control: &StorageReadControl,
    ) -> VersionResult<uqa_storage::mvcc::IdentifierAllocation> {
        if std::thread::current().id() == self.foreground
            && matches!(request, uqa_storage::mvcc::IdentifierRequest::Observe(_))
        {
            if let Some(error) =
                rejected_commit_error(self.identifier_fault.swap(HEALTHY, Ordering::AcqRel))
            {
                return Err(error);
            }
        }
        self.inner.allocate_identifiers(namespace, request, control)
    }
    fn allocate_transaction(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<StorageTransactionId> {
        if std::thread::current().id() == self.foreground {
            self.foreground_transaction_allocations
                .fetch_add(1, Ordering::AcqRel);
        }
        self.inner.allocate_transaction(control)
    }
    fn reclaim_versions(&self, control: &StorageReadControl) -> VersionResult<u64> {
        self.inner.reclaim_versions(control)
    }

    fn snapshot(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<dyn CommittedRecordSnapshot>> {
        self.inner.snapshot(control)
    }
    fn commit(
        &self,
        transaction: StorageTransactionId,
        prepared: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> CommitResult {
        if std::thread::current().id() == self.foreground {
            self.foreground_record_commits
                .fetch_add(1, Ordering::AcqRel);
        }
        let fault = self.fault_for(transaction);
        if let Some(error) = rejected_commit_error(fault) {
            return Err(CommitFailure::Rejected(error));
        }
        if fault == UNAVAILABLE {
            return Err(CommitFailure::Rejected(
                StorageBackendError::Other("injected unavailable receipt read".into()).into(),
            ));
        }
        if !matches!(fault, LOSE_UNCOMMITTED_REPLY | LOSE_CONFLICT_REPLY) {
            let receipt = self.inner.commit(transaction, prepared, control)?;
            if fault == HEALTHY {
                return Ok(receipt);
            }
        }
        self.fault.store(UNAVAILABLE, Ordering::Release);
        Err(CommitFailure::Indeterminate {
            transaction,
            source: if fault == LOSE_CONFLICT_REPLY {
                rejected_commit_error(REJECT_DEPENDENCY)
                    .unwrap()
                    .into_storage_error()
            } else {
                StorageBackendError::Other("injected lost native commit reply".into())
            },
        })
    }
    fn commit_status(
        &self,
        transaction: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        if self.fault_for(transaction) == UNAVAILABLE {
            return Err(
                StorageBackendError::Other("injected unavailable receipt read".into()).into(),
            );
        }
        self.inner.commit_status(transaction, control)
    }
    fn abort(
        &self,
        transaction: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        let fault = self.fault_for(transaction);
        if *self.attempt.lock().unwrap() == Some(transaction) {
            self.aborts.fetch_add(1, Ordering::AcqRel);
        }
        if fault == UNAVAILABLE {
            return Err(StorageBackendError::Other("injected unavailable abort".into()).into());
        }
        let status = self.inner.abort(transaction, control)?;
        if matches!(fault, LOSE_ABORT_REPLY | HIDE_AFTER_ABORT_RECEIPT) {
            self.fault.store(UNAVAILABLE, Ordering::Release);
        }
        if fault == LOSE_ABORT_REPLY {
            return Err(
                StorageBackendError::Other("injected lost native abort reply".into()).into(),
            );
        }
        Ok(status)
    }
}

fn fixtures() -> (tempfile::TempDir, Vec<Arc<FaultPersistence>>) {
    let directory = tempfile::tempdir().unwrap();
    let mut records: Vec<Arc<dyn VersionedPersistence>> = Vec::new();
    for mode in ["plain", "encrypted", "compressed", "compressed-encrypted"] {
        let path = directory.path().join(format!("{mode}.db"));
        let connection = match mode {
            "plain" => ManagedConnection::open(&path),
            "encrypted" => ManagedConnection::open_encrypted(&path, "commit test key"),
            "compressed" => {
                ManagedConnection::open_compressed(&path, SQLiteCompressionOptions::default())
            }
            _ => ManagedConnection::open_compressed_encrypted(
                &path,
                "commit test key",
                SQLiteCompressionOptions::default(),
            ),
        }
        .unwrap();
        records.push(Arc::new(SQLiteRecordStore::new(&connection).unwrap()));
    }
    let redb = RedbStorage::open(directory.path().join("receipt.redb")).unwrap();
    records.push(Arc::new(redb.record_store().unwrap()));
    (
        directory,
        records
            .into_iter()
            .map(|inner| {
                Arc::new(FaultPersistence {
                    inner,
                    fault: AtomicU8::new(HEALTHY),
                    identifier_fault: AtomicU8::new(HEALTHY),
                    serializable_fault: AtomicU8::new(HEALTHY),
                    serializable_completion: Mutex::new(None),
                    foreground: std::thread::current().id(),
                    foreground_transaction_allocations: AtomicUsize::new(0),
                    foreground_record_commits: AtomicUsize::new(0),
                    attempt: Mutex::new(None),
                    aborts: AtomicUsize::new(0),
                })
            })
            .collect(),
    )
}

fn engine(persistence: Arc<FaultPersistence>) -> Engine {
    let store: Arc<dyn KeyValueStore> = Arc::new(VersionedKeyValueStore::new(
        persistence,
        None,
        VersionedSessionOptions::default(),
    ));
    Engine::from_persistent_backends(
        Arc::new(KeyValueCatalog::new(store.clone())),
        Arc::new(KeyValueStorageBackend::new(store)),
    )
    .unwrap()
}

fn assert_unknown(error: &SQLError) {
    assert_eq!(error.sqlstate(), Some("08007"), "{error}");
}

fn count(engine: &Engine, table: &str) -> Value {
    engine
        .sql(&format!("SELECT count(*) AS n FROM {table}"), &[])
        .unwrap()
        .rows[0]["n"]
        .clone()
}

#[test]
fn an_independent_writer_cannot_claim_the_foreground_commit_fault() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        root.sql("CREATE TABLE items(id INTEGER)", &[]).unwrap();
        let independent = root.new_session().unwrap();
        persistence
            .fault
            .store(LOSE_COMMITTED_REPLY, Ordering::Release);
        std::thread::spawn(move || {
            independent
                .sql("INSERT INTO items VALUES (1)", &[])
                .unwrap()
        })
        .join()
        .unwrap();
        assert!(persistence.attempt.lock().unwrap().is_none());
        assert_unknown(&root.sql("INSERT INTO items VALUES (2)", &[]).unwrap_err());
        assert!(root.pending_commit().is_some());
        persistence.fault.store(HEALTHY, Ordering::Release);
        root.commit().unwrap();
        assert_eq!(count(&root, "items"), Value::Int(2));
    }
}

#[test]
fn retained_commit_resolves_without_replaying_preparation_and_publishes_once() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        root.register_scalar_function_with_options(
            "commit_probe",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |_: &[Value]| {
                callback_calls.fetch_add(1, Ordering::AcqRel);
                Ok(Value::Int(1))
            },
        )
        .unwrap();
        root.sql("CREATE TABLE items(id INTEGER); CREATE TABLE audit(id INTEGER); CREATE FUNCTION record_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM commit_probe(); INSERT INTO audit VALUES (NEW.id); RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER deferred_audit AFTER INSERT ON items DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION record_commit(); CREATE TEMP TABLE scratch(id INTEGER) ON COMMIT DELETE ROWS", &[]).unwrap();
        let listener = root.new_session().unwrap();
        listener.sql("LISTEN commit_events", &[]).unwrap();
        root.sql("BEGIN; SAVEPOINT before_write; INSERT INTO items VALUES (7); INSERT INTO scratch VALUES (9); NOTIFY commit_events, 'once'; DECLARE held CURSOR WITH HOLD FOR SELECT commit_probe() AS n", &[]).unwrap();
        persistence
            .fault
            .store(LOSE_COMMITTED_REPLY, Ordering::Release);
        assert_unknown(&root.sql("COMMIT", &[]).unwrap_err());
        let identity = root.pending_commit().expect("retained commit identity");
        assert!(!root.transaction_failed());
        assert_eq!(calls.load(Ordering::Acquire), 2);
        assert_eq!(root.transaction_depth(), 1);
        for sql in [
            "SELECT * FROM items",
            "BEGIN",
            "SAVEPOINT forbidden",
            "ROLLBACK TO before_write",
            "COMMIT",
        ] {
            assert_unknown(&root.sql(sql, &[]).unwrap_err());
            assert_eq!(root.pending_commit(), Some(identity));
        }
        let expected = root.sql("SELECT 1", &[]).unwrap_err();
        assert_unknown(&expected);
        for read in super::serializable_observations::catalog_reads::CatalogRead::ALL {
            let error = read
                .read(&root)
                .expect_err("catalog query must not bypass unresolved completion");
            error.assert_transaction_error(&expected);
            assert_eq!(root.pending_commit(), Some(identity));
        }
        for read in super::serializable_observations::exact_reads::ExactRead::ALL {
            let error = read
                .read(&root, "items", 1)
                .expect_err("exact query bypassed unresolved completion");
            assert_unknown(&error);
            assert_eq!(error.to_string(), expected.to_string());
            assert_eq!(root.pending_commit(), Some(identity));
        }
        for read in super::serializable_observations::statistics_reads::StatisticsRead::ALL {
            read.read(&root)
                .expect_err("statistics query must not bypass unresolved completion")
                .assert_transaction_error(&expected);
            assert_eq!(root.pending_commit(), Some(identity));
        }
        assert_eq!(calls.load(Ordering::Acquire), 2);
        assert_eq!(persistence.aborts.load(Ordering::Acquire), 0);
        persistence.fault.store(HEALTHY, Ordering::Release);
        assert_eq!(
            root.sql("COMMIT", &[]).unwrap().command_tag.as_deref(),
            Some("COMMIT")
        );
        assert_eq!(root.pending_commit(), None);
        assert_eq!(root.transaction_depth(), 0);
        assert_eq!(calls.load(Ordering::Acquire), 2);
        assert_eq!(count(&root, "items"), Value::Int(1));
        assert_eq!(count(&root, "audit"), Value::Int(1));
        assert_eq!(count(&root, "scratch"), Value::Int(0));
        assert_eq!(
            root.sql("FETCH ALL FROM held", &[]).unwrap().rows[0]["n"],
            Value::Int(1)
        );
        listener.poll_sql_notifications().unwrap();
        let notifications = listener.take_sql_notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].payload, "once");
        listener.poll_sql_notifications().unwrap();
        assert!(listener.take_sql_notifications().is_empty());
        assert_eq!(count(&listener, "items"), Value::Int(1));
        let reopened = engine(persistence);
        assert_eq!(count(&reopened, "audit"), Value::Int(1));
    }
}

#[test]
fn rollback_of_a_committed_attempt_finishes_publication_and_reports_the_commit() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        root.sql("CREATE TABLE items(id INTEGER)", &[]).unwrap();
        let listener = root.new_session().unwrap();
        listener.sql("LISTEN commit_events", &[]).unwrap();
        root.sql(
            "BEGIN; INSERT INTO items VALUES (1); NOTIFY commit_events, 'committed'",
            &[],
        )
        .unwrap();
        persistence
            .fault
            .store(LOSE_COMMITTED_REPLY, Ordering::Release);
        assert_unknown(&root.commit().unwrap_err());
        persistence
            .fault
            .store(HIDE_AFTER_ABORT_RECEIPT, Ordering::Release);
        let error = root.rollback().unwrap_err();
        assert_eq!(error.sqlstate(), Some("25000"), "{error}");
        assert!(error.to_string().contains("already committed"));
        assert_eq!(root.transaction_depth(), 0);
        assert_eq!(root.pending_commit(), None);
        assert_eq!(count(&listener, "items"), Value::Int(1));
        listener.poll_sql_notifications().unwrap();
        assert_eq!(listener.take_sql_notifications().len(), 1);
        persistence.fault.store(HEALTHY, Ordering::Release);
        root.sql("INSERT INTO items VALUES (2)", &[]).unwrap();
        assert_eq!(count(&root, "items"), Value::Int(2));
    }
}

#[test]
fn rollback_resolves_an_uncommitted_attempt_without_publishing_private_effects() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        root.sql("CREATE TABLE items(id INTEGER)", &[]).unwrap();
        let listener = root.new_session().unwrap();
        listener.sql("LISTEN commit_events", &[]).unwrap();
        root.sql(
            "BEGIN; INSERT INTO items VALUES (1); NOTIFY commit_events, 'discarded'",
            &[],
        )
        .unwrap();
        persistence
            .fault
            .store(LOSE_UNCOMMITTED_REPLY, Ordering::Release);
        assert_unknown(&root.commit().unwrap_err());
        assert_unknown(&root.rollback().unwrap_err());
        assert!(root.pending_commit().is_some());
        persistence.fault.store(HEALTHY, Ordering::Release);
        root.rollback().unwrap();
        assert_eq!(root.transaction_depth(), 0);
        assert_eq!(root.pending_commit(), None);
        assert_eq!(count(&root, "items"), Value::Int(0));
        assert_eq!(count(&listener, "items"), Value::Int(0));
        listener.poll_sql_notifications().unwrap();
        assert!(listener.take_sql_notifications().is_empty());
    }
}

#[test]
fn a_scoped_callback_leaves_its_uncertain_commit_with_the_session() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        root.sql("CREATE TABLE items(id INTEGER)", &[]).unwrap();
        let mut calls = 0;
        let error = root
            .transaction(|session| {
                calls += 1;
                session.sql("INSERT INTO items VALUES (1)", &[])?;
                persistence
                    .fault
                    .store(LOSE_COMMITTED_REPLY, Ordering::Release);
                Ok(())
            })
            .unwrap_err();
        assert_unknown(&error);
        assert!(root.pending_commit().is_some());
        assert_eq!(persistence.aborts.load(Ordering::Acquire), 0);
        persistence.fault.store(HEALTHY, Ordering::Release);
        root.commit().unwrap();
        assert_eq!(calls, 1);
        assert_eq!(count(&root, "items"), Value::Int(1));
    }
}

#[test]
fn autocommit_resolution_can_chain_without_changing_the_commit_tag() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        root.sql("CREATE TABLE items(id INTEGER)", &[]).unwrap();
        persistence
            .fault
            .store(LOSE_COMMITTED_REPLY, Ordering::Release);
        assert_unknown(&root.sql("INSERT INTO items VALUES (1)", &[]).unwrap_err());
        assert!(root.pending_commit().is_some());
        persistence.fault.store(HEALTHY, Ordering::Release);
        let result = root.sql("COMMIT AND CHAIN", &[]).unwrap();
        assert_eq!(result.command_tag.as_deref(), Some("COMMIT"));
        assert_eq!(root.transaction_depth(), 1);
        assert_eq!(root.pending_commit(), None);
        assert_eq!(count(&root, "items"), Value::Int(1));
        root.rollback().unwrap();
    }
}

#[test]
fn an_implicit_batch_preserves_a_pending_commit_instead_of_running_error_rollback() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        root.sql("CREATE TABLE items(id INTEGER)", &[]).unwrap();
        persistence
            .fault
            .store(LOSE_COMMITTED_REPLY, Ordering::Release);
        assert_unknown(
            &root
                .sql(
                    "INSERT INTO items VALUES (1); COMMIT; INSERT INTO items VALUES (2)",
                    &[],
                )
                .unwrap_err(),
        );
        assert!(root.pending_commit().is_some());
        assert_eq!(persistence.aborts.load(Ordering::Acquire), 0);
        persistence.fault.store(HEALTHY, Ordering::Release);
        root.commit().unwrap();
        assert_eq!(count(&root, "items"), Value::Int(1));
    }
}

#[test]
fn a_lost_abort_reply_resolves_as_a_known_abort_and_restores_private_catalog_changes() {
    let (_directory, fixtures) = fixtures();
    for persistence in fixtures {
        let root = engine(persistence.clone());
        root.sql("CREATE TABLE items(id INTEGER)", &[]).unwrap();
        root.sql(
            "BEGIN; CREATE TABLE discarded(id INTEGER); INSERT INTO items VALUES (1)",
            &[],
        )
        .unwrap();
        persistence
            .fault
            .store(LOSE_UNCOMMITTED_REPLY, Ordering::Release);
        assert_unknown(&root.commit().unwrap_err());
        persistence.fault.store(LOSE_ABORT_REPLY, Ordering::Release);
        assert_unknown(&root.rollback().unwrap_err());
        assert!(root.pending_commit().is_some());
        assert!(!root.transaction_failed());
        persistence.fault.store(HEALTHY, Ordering::Release);
        let error = root.commit().unwrap_err();
        assert_eq!(error.sqlstate(), Some("25000"), "{error}");
        assert!(error.to_string().contains("already aborted"));
        assert_eq!(root.transaction_depth(), 0);
        assert_eq!(root.pending_commit(), None);
        assert_eq!(count(&root, "items"), Value::Int(0));
        assert_eq!(
            root.sql("SELECT * FROM discarded", &[])
                .unwrap_err()
                .sqlstate(),
            Some("42P01")
        );
        root.sql("INSERT INTO items VALUES (2)", &[]).unwrap();
        assert_eq!(count(&root, "items"), Value::Int(1));
    }
}

#[test]
fn temporary_role_dependencies_follow_failed_and_resolved_commit_outcomes() {
    for fault in [REJECT_OTHER, LOSE_COMMITTED_REPLY, LOSE_UNCOMMITTED_REPLY] {
        let (_directory, fixtures) = fixtures();
        for persistence in fixtures {
            let root = engine(persistence.clone());
            root.sql("CREATE ROLE previous_owner; CREATE ROLE added_owner; CREATE TABLE durable(v int); CREATE TEMP TABLE existing(v int); ALTER TABLE existing OWNER TO previous_owner", &[]).unwrap();
            let peer = root.new_session().unwrap();
            root.sql("BEGIN; INSERT INTO durable VALUES(1); DROP TABLE existing; SET LOCAL ROLE added_owner; CREATE TEMP TABLE added(v int)", &[]).unwrap();
            persistence.fault.store(fault, Ordering::Release);
            let failure = root.sql("COMMIT", &[]).unwrap_err();
            if fault == REJECT_OTHER {
                assert_eq!(failure.sqlstate(), Some("XX000"));
            } else {
                assert_unknown(&failure);
                assert!(root.pending_commit().is_some());
            }
            persistence.fault.store(HEALTHY, Ordering::Release);
            if fault != REJECT_OTHER {
                root.sql(
                    if fault == LOSE_COMMITTED_REPLY {
                        "COMMIT"
                    } else {
                        "ROLLBACK"
                    },
                    &[],
                )
                .unwrap();
            }
            assert!(root.pending_commit().is_none());
            assert_eq!(root.transaction_depth(), 0);
            let (retained, retired, present, absent) = if fault == LOSE_COMMITTED_REPLY {
                ("added_owner", "previous_owner", "added", "existing")
            } else {
                ("previous_owner", "added_owner", "existing", "added")
            };
            assert_eq!(
                peer.sql(&format!("DROP ROLE {retained}"), &[])
                    .unwrap_err()
                    .sqlstate(),
                Some("2BP01")
            );
            peer.sql(&format!("DROP ROLE {retired}"), &[]).unwrap();
            root.sql(&format!("SELECT * FROM {present}"), &[]).unwrap();
            assert_eq!(
                root.sql(&format!("SELECT * FROM {absent}"), &[])
                    .unwrap_err()
                    .sqlstate(),
                Some("42P01")
            );
            root.sql(&format!("DROP TABLE {present}"), &[]).unwrap();
            peer.sql(&format!("DROP ROLE {retained}"), &[]).unwrap();
        }
    }
}
