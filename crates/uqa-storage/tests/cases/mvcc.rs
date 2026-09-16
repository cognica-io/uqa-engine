//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Conditional record publication and retained-snapshot conformance.

use std::sync::mpsc;
use std::time::Duration;

use uqa_core::memory::{MemoryBudget, MemoryError};
use uqa_storage::mvcc::{
    CommitSequence, MemoryRecordSnapshot, MemoryVersionStore, PreparedRecordCommit, RecordHistory,
    RecordWrite, VersionError,
};
use uqa_storage::read_control::StorageReadControl;

fn sequence(value: u64) -> CommitSequence {
    CommitSequence::from_u64(value)
}

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(1 << 20)
}

fn value(snapshot: &MemoryRecordSnapshot, key: &[u8]) -> Option<Vec<u8>> {
    snapshot
        .get(key)
        .and_then(|version| version.value().map(|value| value.to_vec()))
}

fn write<'a>(key: &'a [u8], expected: Option<CommitSequence>, value: &'a [u8]) -> RecordWrite<'a> {
    RecordWrite {
        key,
        expected,
        value: Some(value),
    }
}

#[test]
fn historical_reads_preserve_deletion_and_recreation_boundaries() {
    let memory = MemoryBudget::new(4096);
    let mut history = RecordHistory::new(&memory);
    history.append(sequence(10), Some("created")).unwrap();
    history.append(sequence(20), None).unwrap();
    history.append(sequence(30), Some("recreated")).unwrap();
    assert!(history.visible_at(sequence(9)).is_none());
    assert_eq!(
        history.visible_at(sequence(10)).unwrap().value(),
        Some(&"created")
    );
    assert_eq!(
        history.visible_at(sequence(19)).unwrap().value(),
        Some(&"created")
    );
    assert!(history.visible_at(sequence(29)).unwrap().value().is_none());
    assert_eq!(
        history.visible_at(sequence(30)).unwrap().value(),
        Some(&"recreated")
    );
    assert_eq!(history.reclaim_before(sequence(25)), 1);
    assert_eq!(history.len(), 2);
    assert_eq!(
        history.visible_at(sequence(25)).unwrap().sequence(),
        sequence(20)
    );
    assert!(history.visible_at(sequence(25)).unwrap().value().is_none());
    assert_eq!(history.reclaim_before(sequence(100)), 1);
    assert_eq!(history.head().unwrap().value(), Some(&"recreated"));
    drop(history);
    assert_eq!(memory.used(), 0);
}

#[test]
fn invalid_or_unaffordable_history_updates_preserve_the_original() {
    let memory = MemoryBudget::new(4096);
    let mut history = RecordHistory::new(&memory);
    assert!(matches!(
        history.append(CommitSequence::INITIAL, Some(0)),
        Err(VersionError::CommitOrder { .. })
    ));
    history.append(sequence(8), Some(1)).unwrap();
    for revision in [7, 8] {
        assert!(matches!(
            history.append(sequence(revision), Some(9)),
            Err(VersionError::CommitOrder { .. })
        ));
    }
    let held = memory.reserve(memory.limit() - memory.used()).unwrap();
    assert!(matches!(
        history.fork_appending(sequence(9), Some(2)),
        Err(VersionError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(history.len(), 1);
    assert_eq!(history.head().unwrap().value(), Some(&1));
    drop(held);
    assert!(matches!(
        sequence(u64::MAX).successor(),
        Err(VersionError::SequenceExhausted)
    ));
}

#[test]
fn disjoint_prepared_records_commit_before_an_older_writer_finishes() {
    let memory = MemoryBudget::new(1 << 20);
    let store = MemoryVersionStore::new(&memory);
    let seeded = store
        .commit(
            &[write(b"row/1", None, b"one"), write(b"row/2", None, b"two")],
            &control(),
        )
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let writer_store = store.clone();
    let writer = std::thread::spawn(move || {
        let snapshot = writer_store.snapshot().unwrap();
        let original = snapshot.get(b"row/1").unwrap().sequence();
        let prepared =
            PreparedRecordCommit::new(&[write(b"row/1", Some(original), b"A")], &control())
                .unwrap();
        ready_tx.send(()).unwrap();
        resume_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(value(&snapshot, b"row/2"), Some(b"two".to_vec()));
        writer_store.commit_prepared(&prepared, &control()).unwrap()
    });
    ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let second = store
        .commit(&[write(b"row/2", Some(seeded), b"B")], &control())
        .unwrap();
    let between = store.snapshot().unwrap();
    assert_eq!(value(&between, b"row/1"), Some(b"one".to_vec()));
    assert_eq!(value(&between, b"row/2"), Some(b"B".to_vec()));
    resume_tx.send(()).unwrap();
    assert!(writer.join().unwrap() > second);
    let current = store.snapshot().unwrap();
    assert_eq!(value(&current, b"row/1"), Some(b"A".to_vec()));
    assert_eq!(value(&current, b"row/2"), Some(b"B".to_vec()));
    assert_eq!(value(&between, b"row/1"), Some(b"one".to_vec()));
    drop((current, between, store));
    assert_eq!(memory.used(), 0);
}

#[test]
fn conflict_on_the_last_record_does_not_publish_earlier_changes() {
    let store = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    let original = store
        .commit(
            &[write(b"a", None, b"old-a"), write(b"b", None, b"old-b")],
            &control(),
        )
        .unwrap();
    let committed = store
        .commit(&[write(b"b", Some(original), b"other")], &control())
        .unwrap();
    let result = store.commit(
        &[
            write(b"a", Some(original), b"stale-a"),
            write(b"b", Some(original), b"stale-b"),
        ],
        &control(),
    );
    assert!(
        matches!(result, Err(VersionError::WriteConflict { mutation: 1, expected: Some(expected), actual: Some(actual) }) if expected == original && actual == committed)
    );
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.sequence(), committed);
    assert_eq!(value(&snapshot, b"a"), Some(b"old-a".to_vec()));
    assert_eq!(value(&snapshot, b"b"), Some(b"other".to_vec()));
}

#[test]
fn tombstones_prevent_an_absent_key_from_hiding_intervening_writes() {
    let store = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    let before = store.snapshot().unwrap();
    assert!(before.get(b"record").is_none());
    let created = store
        .commit(&[write(b"record", None, b"value")], &control())
        .unwrap();
    let deleted = store
        .commit(
            &[RecordWrite {
                key: b"record",
                expected: Some(created),
                value: None,
            }],
            &control(),
        )
        .unwrap();
    drop(before);
    assert_eq!(store.reclaim(), 1);
    assert!(
        matches!(store.commit(&[write(b"record", None, b"stale")], &control()), Err(VersionError::WriteConflict { actual: Some(actual), .. }) if actual == deleted)
    );
    let current = store.snapshot().unwrap();
    let tombstone = current.get(b"record").unwrap();
    assert_eq!(tombstone.sequence(), deleted);
    assert!(tombstone.value().is_none());
    store
        .commit(&[write(b"record", Some(deleted), b"")], &control())
        .unwrap();
    assert_eq!(
        value(&store.snapshot().unwrap(), b"record"),
        Some(Vec::new())
    );
}

#[test]
fn duplicate_record_batches_are_rejected_without_advancing_visibility() {
    let store = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    let result = store.commit(
        &[
            write(b"same", None, b"first"),
            write(b"other", None, b"middle"),
            write(b"same", None, b"last"),
        ],
        &control(),
    );
    assert!(matches!(
        result,
        Err(VersionError::DuplicateRecord {
            first: 0,
            second: 2
        })
    ));
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.sequence(), CommitSequence::INITIAL);
    assert!(snapshot.get(b"same").is_none());
    assert!(snapshot.get(b"other").is_none());
}

#[test]
fn failed_preparation_releases_its_memory_and_preserves_every_record() {
    let memory = MemoryBudget::new(4096);
    let store = MemoryVersionStore::new(&memory);
    let revision = store
        .commit(&[write(b"keep", None, b"original")], &control())
        .unwrap();
    let retained = memory.used();
    let huge = vec![7; 8192];
    let result = store.commit(
        &[
            write(b"keep", Some(revision), b"candidate"),
            write(b"new", None, &huge),
        ],
        &control(),
    );
    assert!(matches!(
        result,
        Err(VersionError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(memory.used(), retained);
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.sequence(), revision);
    assert_eq!(value(&snapshot, b"keep"), Some(b"original".to_vec()));
    assert!(snapshot.get(b"new").is_none());
}

#[test]
fn pinned_pages_and_reclamation_keep_their_original_record_versions() {
    let memory = MemoryBudget::new(1 << 20);
    let store = MemoryVersionStore::new(&memory);
    let first = store
        .commit(
            &[write(b"p/1", None, b"old"), write(b"p/2", None, b"second")],
            &control(),
        )
        .unwrap();
    let pinned = store.snapshot().unwrap();
    let second = store
        .commit(
            &[
                write(b"p/1", Some(first), b"middle"),
                write(b"p/3", None, b"new"),
            ],
            &control(),
        )
        .unwrap();
    store
        .commit(&[write(b"p/1", Some(second), b"latest")], &control())
        .unwrap();
    assert_eq!(store.reclaim(), 0);
    let read = control();
    let page = pinned.scan(b"p/", None, 1, &read).unwrap();
    assert_eq!(&*page[0].key, b"p/1");
    assert_eq!(&***page[0].version.value().unwrap(), b"old");
    let following = pinned.scan(b"p/", Some(&page[0].key), 10, &read).unwrap();
    assert_eq!(following.len(), 1);
    assert_eq!(&*following[0].key, b"p/2");
    assert!(pinned
        .scan(b"p/", Some(b"q/"), 10, &read)
        .unwrap()
        .is_empty());
    drop(pinned);
    assert_eq!(store.reclaim(), 2);
    assert_eq!(&***page[0].version.value().unwrap(), b"old");
    drop((page, following));
    assert_eq!(read.memory().used(), 0);
    assert_eq!(
        value(&store.snapshot().unwrap(), b"p/1"),
        Some(b"latest".to_vec())
    );
    drop(store);
    assert_eq!(memory.used(), 0);
}

#[test]
fn cancellation_and_empty_commits_do_not_publish_a_revision() {
    let store = MemoryVersionStore::new(&MemoryBudget::new(4096));
    assert_eq!(
        store.commit(&[], &control()).unwrap(),
        CommitSequence::INITIAL
    );
    let cancelled = control();
    cancelled.cancellation().cancel();
    assert!(matches!(
        store.commit(&[write(b"x", None, b"cancelled")], &cancelled),
        Err(VersionError::Cancelled(_))
    ));
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.sequence(), CommitSequence::INITIAL);
    assert!(snapshot.get(b"x").is_none());
    assert!(matches!(
        snapshot.scan(b"", None, 10, &cancelled),
        Err(VersionError::Cancelled(_))
    ));
}

#[test]
fn zero_length_pages_require_no_result_allocation() {
    let store = MemoryVersionStore::new(&MemoryBudget::new(4096));
    store
        .commit(&[write(b"x", None, b"value")], &control())
        .unwrap();
    let read = StorageReadControl::with_limit(0);
    assert!(store
        .snapshot()
        .unwrap()
        .scan(b"", None, 0, &read)
        .unwrap()
        .is_empty());
    assert!(matches!(
        store.snapshot().unwrap().scan(b"", None, 1, &read),
        Err(VersionError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(read.memory().used(), 0);
}

#[test]
fn prepared_values_survive_their_inputs_and_discard_does_not_restore_old_state() {
    let store = MemoryVersionStore::new(&MemoryBudget::new(8192));
    let preparation = control();
    let mut bytes = b"original".to_vec();
    let prepared = PreparedRecordCommit::new(&[write(b"a", None, &bytes)], &preparation).unwrap();
    bytes.fill(b'x');
    drop(bytes);
    assert_eq!(prepared.records()[0].value(), Some(b"original".as_slice()));
    let discarded =
        PreparedRecordCommit::new(&[write(b"b", None, b"discarded")], &preparation).unwrap();
    store
        .commit(&[write(b"b", None, b"committed")], &control())
        .unwrap();
    drop(discarded);
    store.commit_prepared(&prepared, &control()).unwrap();
    let snapshot = store.snapshot().unwrap();
    assert_eq!(value(&snapshot, b"a"), Some(b"original".to_vec()));
    assert_eq!(value(&snapshot, b"b"), Some(b"committed".to_vec()));
    drop((snapshot, prepared, store));
    assert_eq!(preparation.memory().used(), 0);
}

#[test]
fn allocation_failure_during_publication_preparation_keeps_the_commit_atomic() {
    let memory = MemoryBudget::new(16384);
    let store = MemoryVersionStore::new(&memory);
    let first = store
        .commit(
            &[write(b"a", None, b"old-a"), write(b"b", None, b"old-b")],
            &control(),
        )
        .unwrap();
    let prepared = PreparedRecordCommit::new(
        &[
            write(b"a", Some(first), b"new-a"),
            write(b"b", Some(first), b"new-b"),
        ],
        &control(),
    )
    .unwrap();
    let mut failures = 0;
    let mut succeeded = false;
    for remaining in (0..=4096).step_by(64) {
        let retained = memory.used();
        let pressure = memory
            .reserve(memory.limit() - retained - remaining)
            .unwrap();
        match store.commit_prepared(&prepared, &control()) {
            Err(VersionError::Memory(MemoryError::Limit { .. })) => {
                failures += 1;
                assert_eq!(memory.used(), retained + pressure.bytes());
                drop(pressure);
                let snapshot = store.snapshot().unwrap();
                assert_eq!(snapshot.sequence(), first);
                assert_eq!(value(&snapshot, b"a"), Some(b"old-a".to_vec()));
                assert_eq!(value(&snapshot, b"b"), Some(b"old-b".to_vec()));
            }
            Ok(sequence) => {
                drop(pressure);
                assert!(sequence > first);
                succeeded = true;
                break;
            }
            Err(error) => panic!("unexpected commit result: {error}"),
        }
    }
    assert!(failures > 1 && succeeded);
    let current = store.snapshot().unwrap();
    assert_eq!(value(&current, b"a"), Some(b"new-a".to_vec()));
    assert_eq!(value(&current, b"b"), Some(b"new-b".to_vec()));
}

#[test]
fn revision_validation_preserves_provider_errors_and_stops_after_cancellation() {
    let prepared = PreparedRecordCommit::new(
        &[write(b"a", None, b"a"), write(b"b", None, b"b")],
        &control(),
    )
    .unwrap();
    let read = control();
    let mut observed = 0;
    let result = prepared.validate(read.cancellation(), |_| {
        observed += 1;
        read.cancellation().cancel();
        Ok(None)
    });
    assert!(matches!(result, Err(VersionError::Cancelled(_))));
    assert_eq!(observed, 1);
    let result = prepared.validate(control().cancellation(), |_| {
        Err(uqa_storage::StorageBackendError::Other("read failed".into()).into())
    });
    assert!(matches!(result, Err(VersionError::Storage(_))));
}

#[test]
fn snapshot_admission_and_collection_cannot_lose_a_visible_revision() {
    let store = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    let seeded = 1_u64.to_be_bytes();
    store
        .commit(&[write(b"counter", None, &seeded)], &control())
        .unwrap();
    let writer_store = store.clone();
    let collector_store = store.clone();
    let writer = std::thread::spawn(move || {
        for next in 2_u64..=128 {
            writer_store
                .commit(
                    &[write(
                        b"counter",
                        Some(sequence(next - 1)),
                        &next.to_be_bytes(),
                    )],
                    &control(),
                )
                .unwrap();
        }
    });
    let collector = std::thread::spawn(move || {
        for _ in 0..256 {
            collector_store.reclaim();
        }
    });
    for _ in 0..256 {
        let snapshot = store.snapshot().unwrap();
        let visible = value(&snapshot, b"counter").unwrap();
        assert_eq!(
            u64::from_be_bytes(visible.try_into().unwrap()),
            snapshot.sequence().as_u64()
        );
    }
    writer.join().unwrap();
    collector.join().unwrap();
    assert_eq!(store.snapshot().unwrap().sequence(), sequence(128));
}
