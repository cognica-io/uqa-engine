//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual redb persistence behind the common versioned-record contract.

use std::sync::{mpsc, Arc};
use std::time::Duration;

use uqa_storage::mvcc::{
    CommitFailure, CommitSequence, CommitStatus, CommittedRecordSnapshot, PreparedRecordCommit,
    PrivateRecordChanges, RecordWrite, StorageTransactionId, VersionError, VersionedPersistence,
};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::StorageSavepointId;
use uqa_storage_redb::RedbStorage;

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(1 << 20)
}

#[test]
fn record_format_upgrade_preserves_history_identity_allocations_and_receipts() {
    use redb::{ReadableDatabase, ReadableTable, TableDefinition};
    const METADATA: TableDefinition<&str, &[u8]> = TableDefinition::new("uqa_mvcc_metadata");
    for format in [1_u64, 2, 3, 4, 5] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("old-record-format.redb");
        let (identity, receipt, pending) = {
            let storage = RedbStorage::open(&path).unwrap();
            let records = storage.record_store().unwrap();
            let id = records.allocate_transaction(&control()).unwrap();
            let receipt = records
                .commit(
                    id,
                    &prepared(b"migration", None, Some(b"preserved")),
                    &control(),
                )
                .unwrap();
            let pending = records.allocate_transaction(&control()).unwrap();
            (records.database_id(), receipt, pending)
        };
        {
            let database = redb::Database::open(&path).unwrap();
            let transaction = database.begin_write().unwrap();
            {
                let mut metadata = transaction.open_table(METADATA).unwrap();
                assert_eq!(
                    metadata.get("format").unwrap().unwrap().value(),
                    6_u64.to_be_bytes()
                );
                metadata
                    .insert("format", format.to_be_bytes().as_slice())
                    .unwrap();
            }
            if format < 5 {
                transaction
                    .delete_table(TableDefinition::<&[u8], &[u8]>::new("uqa_mvcc_identifiers"))
                    .unwrap();
            } else {
                transaction
                    .open_table(TableDefinition::<&[u8], &[u8]>::new("uqa_mvcc_identifiers"))
                    .unwrap()
                    .insert(
                        b"migration-identities".as_slice(),
                        999_u64.to_be_bytes().as_slice(),
                    )
                    .unwrap();
            }
            transaction.commit().unwrap();
        }
        {
            let storage = RedbStorage::open(&path).unwrap();
            let records = storage.record_store().unwrap();
            assert_eq!(records.database_id(), identity);
            if format == 5 {
                assert_eq!(
                    records
                        .allocate_identifiers(
                            b"migration-identities",
                            uqa_storage::mvcc::IdentifierRequest::Observe(0),
                            &control()
                        )
                        .unwrap()
                        .watermark(),
                    999
                );
            }
            assert_eq!(
                records
                    .commit_status(receipt.transaction, &control())
                    .unwrap(),
                CommitStatus::Committed(receipt)
            );
            assert_eq!(
                records.commit_status(pending, &control()).unwrap(),
                CommitStatus::Pending
            );
            let snapshot = records.snapshot(&control()).unwrap();
            assert_eq!(snapshot.sequence(), receipt.sequence);
            assert_eq!(value(&*snapshot, b"migration"), Some(b"preserved".to_vec()));
            assert!(
                records
                    .allocate_transaction(&control())
                    .unwrap()
                    .allocation()
                    > pending.allocation()
            );
        }
        let database = redb::Database::open(&path).unwrap();
        let transaction = database.begin_read().unwrap();
        assert_eq!(
            transaction
                .open_table(METADATA)
                .unwrap()
                .get("format")
                .unwrap()
                .unwrap()
                .value(),
            6_u64.to_be_bytes()
        );
    }
}

fn prepared(
    key: &[u8],
    expected: Option<CommitSequence>,
    value: Option<&[u8]>,
) -> PreparedRecordCommit {
    PreparedRecordCommit::new(
        &[RecordWrite {
            key,
            expected,
            value,
        }],
        &control(),
    )
    .unwrap()
}

fn value(snapshot: &dyn CommittedRecordSnapshot, key: &[u8]) -> Option<Vec<u8>> {
    snapshot
        .get(key, &control())
        .unwrap()
        .and_then(|record| record.value().map(|value| value.to_vec()))
}

#[test]
fn independent_commit_finishes_while_another_writer_keeps_private_changes() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("writers.redb")).unwrap();
    let records = Arc::new(storage.record_store().unwrap());
    let control = control();
    let a = records.allocate_transaction(&control).unwrap();
    let private = PrivateRecordChanges::new(control.memory());
    private
        .apply(
            &[RecordWrite {
                key: b"a",
                expected: None,
                value: Some(b"keep"),
            }],
            &control,
        )
        .unwrap();
    let keep = StorageSavepointId::allocate();
    private.savepoint(keep).unwrap();
    private
        .apply(
            &[RecordWrite {
                key: b"a",
                expected: None,
                value: Some(b"undo"),
            }],
            &control,
        )
        .unwrap();
    let before = records.snapshot(&control).unwrap();
    let (finished, wait) = mpsc::channel();
    let writer = Arc::clone(&records);
    let handle = std::thread::spawn(move || {
        let control = self::control();
        let b = writer.allocate_transaction(&control).unwrap();
        let receipt = writer
            .commit(b, &prepared(b"b", None, Some(b"other")), &control)
            .unwrap();
        finished.send(receipt).unwrap();
    });
    let b = wait
        .recv_timeout(Duration::from_secs(10))
        .expect("B must finish before A ends");
    handle.join().unwrap();
    assert!(value(&*before, b"b").is_none());
    assert_eq!(
        records.commit_status(a, &control).unwrap(),
        CommitStatus::Pending
    );
    assert_eq!(
        value(&*records.snapshot(&control).unwrap(), b"b").unwrap(),
        b"other"
    );
    private.rollback_to_savepoint(keep).unwrap();
    let a = records
        .commit(a, &private.prepare(&control).unwrap(), &control)
        .unwrap();
    assert!(a.sequence > b.sequence);
    private.rollback().unwrap();
    let current = records.snapshot(&control).unwrap();
    assert_eq!(value(&*current, b"a").unwrap(), b"keep");
    assert_eq!(value(&*current, b"b").unwrap(), b"other");
}

#[test]
fn conflicting_last_record_rejects_every_change_and_keeps_a_known_pending_outcome() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("conflict.redb")).unwrap();
    let records = storage.record_store().unwrap();
    let control = control();
    let first = records.allocate_transaction(&control).unwrap();
    let receipt = records
        .commit(first, &prepared(b"z", None, Some(b"old")), &control)
        .unwrap();
    let stale = records.allocate_transaction(&control).unwrap();
    let changes = PreparedRecordCommit::new(
        &[
            RecordWrite {
                key: b"a",
                expected: None,
                value: Some(b"must not appear"),
            },
            RecordWrite {
                key: b"z",
                expected: Some(receipt.sequence),
                value: None,
            },
        ],
        &control,
    )
    .unwrap();
    let other = records.allocate_transaction(&control).unwrap();
    let latest = records
        .commit(
            other,
            &prepared(b"z", Some(receipt.sequence), Some(b"winner")),
            &control,
        )
        .unwrap();
    assert!(matches!(
        records.commit(stale, &changes, &control),
        Err(CommitFailure::Rejected(VersionError::WriteConflict {
            mutation: 1,
            ..
        }))
    ));
    let snapshot = records.snapshot(&control).unwrap();
    assert_eq!(snapshot.sequence(), latest.sequence);
    assert!(value(&*snapshot, b"a").is_none());
    assert_eq!(value(&*snapshot, b"z").unwrap(), b"winner");
    assert_eq!(
        records.commit_status(stale, &control).unwrap(),
        CommitStatus::Pending
    );
    assert_eq!(
        records.abort(stale, &control).unwrap(),
        CommitStatus::Aborted
    );
    assert!(matches!(
        records.commit(stale, &changes, &control),
        Err(CommitFailure::Rejected(VersionError::TransactionFinished))
    ));
}

#[test]
fn receipt_retry_and_abort_never_replay_a_committed_replacement_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("receipts.redb");
    let control = control();
    let original = prepared(b"key", None, Some(b"original"));
    let (receipt, later) = {
        let storage = RedbStorage::open(&path).unwrap();
        let records = storage.record_store().unwrap();
        let id = records.allocate_transaction(&control).unwrap();
        let receipt = records.commit(id, &original, &control).unwrap();
        let next = records.allocate_transaction(&control).unwrap();
        let later = records
            .commit(
                next,
                &prepared(b"key", Some(receipt.sequence), Some(b"later")),
                &control,
            )
            .unwrap();
        (receipt, later)
    };
    let storage = RedbStorage::open(&path).unwrap();
    let records = storage.record_store().unwrap();
    assert_eq!(
        records
            .commit(receipt.transaction, &original, &control)
            .unwrap(),
        receipt
    );
    assert_eq!(
        records.abort(receipt.transaction, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    assert!(matches!(
        records.commit(
            receipt.transaction,
            &prepared(b"key", None, Some(b"different")),
            &control
        ),
        Err(CommitFailure::Rejected(VersionError::CommitMismatch))
    ));
    let snapshot = records.snapshot(&control).unwrap();
    assert_eq!(snapshot.sequence(), later.sequence);
    assert_eq!(value(&*snapshot, b"key").unwrap(), b"later");
    assert!(
        records.allocate_transaction(&control).unwrap().allocation()
            > later.transaction.allocation()
    );
}

#[test]
fn allocation_abort_and_empty_commit_do_not_advance_record_visibility() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("empty.redb")).unwrap();
    let records = storage.record_store().unwrap();
    let control = control();
    let one = records.allocate_transaction(&control).unwrap();
    let two = records.allocate_transaction(&control).unwrap();
    assert_ne!(one, two);
    assert_eq!(records.abort(one, &control).unwrap(), CommitStatus::Aborted);
    let empty = PreparedRecordCommit::new(&[], &control).unwrap();
    let receipt = records.commit(two, &empty, &control).unwrap();
    assert_eq!(receipt.sequence, CommitSequence::INITIAL);
    assert_eq!(
        records.snapshot(&control).unwrap().sequence(),
        CommitSequence::INITIAL
    );
    assert_eq!(
        records.commit_status(two, &control).unwrap(),
        CommitStatus::Committed(receipt)
    );
    let unknown = StorageTransactionId::new(records.database_id(), two.allocation() + 10).unwrap();
    assert_eq!(
        records.commit_status(unknown, &control).unwrap(),
        CommitStatus::Unknown
    );
    assert_eq!(
        records.abort(unknown, &control).unwrap(),
        CommitStatus::Unknown
    );
    assert!(matches!(
        records.commit(unknown, &empty, &control),
        Err(CommitFailure::Rejected(VersionError::UnknownTransaction))
    ));
}

#[test]
fn historical_binary_pages_keep_tombstones_and_do_not_pin_native_transactions() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("history.redb")).unwrap();
    let records = storage.record_store().unwrap();
    let control = control();
    let empty = records.snapshot(&control).unwrap();
    let keys = [b"".as_slice(), b"a", b"a\0", b"a\xff", b"b", b"\xff"];
    let writes: Vec<_> = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: None,
            value: Some(key),
        })
        .collect();
    let id = records.allocate_transaction(&control).unwrap();
    let first = records
        .commit(
            id,
            &PreparedRecordCommit::new(&writes, &control).unwrap(),
            &control,
        )
        .unwrap();
    let before = records.snapshot(&control).unwrap();
    let id = records.allocate_transaction(&control).unwrap();
    records
        .commit(id, &prepared(b"a\0", Some(first.sequence), None), &control)
        .unwrap();
    let deleted = records.snapshot(&control).unwrap();
    let deleted_revision = deleted.get(b"a\0", &control).unwrap().unwrap().sequence();
    let id = records.allocate_transaction(&control).unwrap();
    records
        .commit(
            id,
            &prepared(b"a\0", Some(deleted_revision), Some(b"reborn")),
            &control,
        )
        .unwrap();
    assert!(empty.scan(b"", None, 10, &control).unwrap().is_empty());
    assert_eq!(value(&*before, b"a\0").unwrap(), b"a\0");
    assert!(deleted
        .get(b"a\0", &control)
        .unwrap()
        .unwrap()
        .value()
        .is_none());
    let page = deleted.scan(b"a", Some(b"a"), 2, &control).unwrap();
    assert_eq!(&*page[0].key, b"a\0");
    assert!(page[0].version.value().is_none());
    assert_eq!(&*page[1].key, b"a\xff");
    assert!(deleted
        .scan(b"a", Some(b"b"), 2, &control)
        .unwrap()
        .is_empty());
    assert_eq!(
        before.scan(b"", None, 10, &control).unwrap().len(),
        keys.len()
    );
}

#[test]
fn cancellation_and_read_limits_leave_the_provider_usable() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("limits.redb")).unwrap();
    let records = storage.record_store().unwrap();
    let control = control();
    let id = records.allocate_transaction(&control).unwrap();
    let payload = vec![42; 65537];
    let writes = prepared(b"key", None, Some(&payload));
    control.cancellation().cancel();
    assert!(matches!(
        records.commit(id, &writes, &control),
        Err(CommitFailure::Rejected(VersionError::Cancelled(_)))
    ));
    control.cancellation().reset();
    assert_eq!(
        records.commit_status(id, &control).unwrap(),
        CommitStatus::Pending
    );
    records.commit(id, &writes, &control).unwrap();
    let snapshot = records.snapshot(&control).unwrap();
    let small = StorageReadControl::with_limit(128);
    assert!(matches!(
        snapshot.get(b"key", &small),
        Err(VersionError::Memory(_))
    ));
    assert!(matches!(
        snapshot.scan(b"", None, 1, &small),
        Err(VersionError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(value(&*snapshot, b"key").unwrap(), payload);
    drop(snapshot);
    let allowance = StorageReadControl::with_limit(256);
    let snapshot = records.snapshot(&allowance).unwrap();
    assert!(allowance.memory().used() > 0);
    drop(snapshot);
    assert_eq!(allowance.memory().used(), 0);
    assert!(matches!(
        records.snapshot(&StorageReadControl::with_limit(0)),
        Err(VersionError::Memory(_))
    ));
}

#[test]
fn transaction_affinity_rejects_an_allocation_from_another_database() {
    let directory = tempfile::tempdir().unwrap();
    let a = RedbStorage::open(directory.path().join("a.redb")).unwrap();
    let b = RedbStorage::open(directory.path().join("b.redb")).unwrap();
    let a = a.record_store().unwrap();
    let b = b.record_store().unwrap();
    let control = control();
    let id = a.allocate_transaction(&control).unwrap();
    assert!(matches!(
        b.commit(id, &prepared(b"key", None, Some(b"value")), &control),
        Err(CommitFailure::Rejected(VersionError::WrongDatabase))
    ));
    assert!(matches!(
        b.commit_status(id, &control),
        Err(VersionError::WrongDatabase)
    ));
    assert!(matches!(
        b.abort(id, &control),
        Err(VersionError::WrongDatabase)
    ));
    assert_eq!(
        a.commit_status(id, &control).unwrap(),
        CommitStatus::Pending
    );
}
