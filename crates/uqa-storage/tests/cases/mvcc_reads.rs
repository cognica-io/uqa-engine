//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Committed/private snapshot composition through the common reader contract.

use std::collections::BTreeMap;
use std::sync::Arc;

use proptest::prelude::*;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::{
    CommitSequence, CommittedRecordSnapshot, MemoryRecordSnapshot, MemoryVersionStore,
    MergedRecordSnapshot, PrivateRecordChanges, RecordVersion, RecordWrite, ScannedRecord,
    SharedRecordValue, VersionError, VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

fn control() -> StorageReadControl {
    StorageReadControl::with_limit(1 << 20)
}

fn write<'a>(
    key: &'a [u8],
    expected: Option<CommitSequence>,
    value: Option<&'a [u8]>,
) -> RecordWrite<'a> {
    RecordWrite {
        key,
        expected,
        value,
    }
}

fn view(store: &MemoryVersionStore, private: &PrivateRecordChanges) -> MergedRecordSnapshot {
    MergedRecordSnapshot::new(
        Arc::new(store.snapshot().unwrap()),
        private.snapshot().unwrap(),
    )
}

fn value(view: &MergedRecordSnapshot, key: &[u8]) -> Option<Vec<u8>> {
    view.get(key, &control())
        .unwrap()
        .and_then(|record| record.value().map(<[u8]>::to_vec))
}

#[test]
fn refreshing_a_committed_boundary_preserves_own_writes_and_older_commands() {
    let control = control();
    let store = MemoryVersionStore::new(control.memory());
    let initial = store
        .commit(
            &[
                write(b"a", None, Some(b"one")),
                write(b"c", None, Some(b"three")),
            ],
            &control,
        )
        .unwrap();
    let private = PrivateRecordChanges::new(control.memory());
    let original = view(&store, &private);
    private
        .apply(
            &[
                write(b"a", Some(initial), Some(b"mine")),
                write(b"b", None, Some(b"insert")),
            ],
            &control,
        )
        .unwrap();
    let command = view(&store, &private);
    let other = store
        .commit(
            &[
                write(b"c", Some(initial), Some(b"other")),
                write(b"d", None, Some(b"new")),
            ],
            &control,
        )
        .unwrap();
    let later = view(&store, &private);
    assert_eq!(command.sequence(), initial);
    assert_eq!(later.sequence(), other);
    assert_eq!(value(&original, b"a").unwrap(), b"one");
    assert!(value(&original, b"b").is_none());
    assert_eq!(value(&command, b"a").unwrap(), b"mine");
    assert_eq!(value(&command, b"c").unwrap(), b"three");
    assert!(value(&command, b"d").is_none());
    assert_eq!(value(&later, b"a").unwrap(), b"mine");
    assert_eq!(value(&later, b"c").unwrap(), b"other");
    assert_eq!(
        later
            .get(b"a", &control)
            .unwrap()
            .unwrap()
            .original_revision(),
        Some(initial)
    );
    private.rollback().unwrap();
    assert_eq!(value(&view(&store, &private), b"a").unwrap(), b"one");
    assert_eq!(value(&later, b"a").unwrap(), b"mine");
    assert_eq!(value(&later, b"d").unwrap(), b"new");
}

#[test]
fn point_reads_distinguish_committed_absence_and_both_kinds_of_tombstone() {
    let control = control();
    let store = MemoryVersionStore::new(control.memory());
    let committed = store
        .commit(
            &[
                write(b"gone", None, None),
                write(b"live", None, Some(b"value")),
            ],
            &control,
        )
        .unwrap();
    let private = PrivateRecordChanges::new(control.memory());
    private
        .apply(&[write(b"live", Some(committed), None)], &control)
        .unwrap();
    let view = view(&store, &private);
    let gone = view.get(b"gone", &control).unwrap().unwrap();
    assert_eq!(gone.original_revision(), Some(committed));
    assert!(gone.value().is_none());
    assert!(!gone.is_private());
    let deleted = view.get(b"live", &control).unwrap().unwrap();
    assert_eq!(deleted.original_revision(), Some(committed));
    assert!(deleted.value().is_none());
    assert!(deleted.is_private());
    assert!(view.get(b"absent", &control).unwrap().is_none());
}

#[test]
fn overlapping_ordered_pages_remain_fixed_across_commits_and_private_undo() {
    let control = control();
    let store = MemoryVersionStore::new(control.memory());
    let committed = store
        .commit(
            &[
                write(b"r/a", None, Some(b"a")),
                write(b"r/c", None, Some(b"c")),
                write(b"r/e", None, Some(b"e")),
            ],
            &control,
        )
        .unwrap();
    let private = PrivateRecordChanges::new(control.memory());
    private
        .apply(
            &[
                write(b"r/b", None, Some(b"b")),
                write(b"r/c", Some(committed), None),
                write(b"r/z", None, Some(b"z")),
            ],
            &control,
        )
        .unwrap();
    let snapshot = view(&store, &private);
    let first = snapshot.scan(b"r/", None, 2, &control).unwrap();
    assert_eq!(&*first[0].key, b"r/a");
    assert_eq!(&*first[1].key, b"r/b");
    store
        .commit(
            &[
                write(b"r/d", None, Some(b"new")),
                write(b"r/e", Some(committed), None),
            ],
            &control,
        )
        .unwrap();
    private.rollback().unwrap();
    private
        .apply(&[write(b"r/cc", None, Some(b"later"))], &control)
        .unwrap();
    store.reclaim().unwrap();
    let second = snapshot
        .scan(b"r/", Some(&first[1].key), 2, &control)
        .unwrap();
    assert_eq!(&*second[0].key, b"r/c");
    assert!(second[0].record.value().is_none());
    assert_eq!(&*second[1].key, b"r/e");
    assert_eq!(second[1].record.value().unwrap(), b"e");
    let last = snapshot
        .scan(b"r/", Some(&second[1].key), 2, &control)
        .unwrap();
    assert_eq!(last.len(), 1);
    assert_eq!(&*last[0].key, b"r/z");
    assert!(snapshot
        .scan(b"r/", Some(&last[0].key), 2, &control)
        .unwrap()
        .is_empty());
    assert!(snapshot
        .scan(b"r/", Some(b"s"), 2, &control)
        .unwrap()
        .is_empty());
}

struct InterruptedSnapshot {
    source: MemoryRecordSnapshot,
    cancel_after_scan: bool,
}

impl CommittedRecordSnapshot for InterruptedSnapshot {
    fn sequence(&self) -> CommitSequence {
        self.source.sequence()
    }

    fn get(
        &self,
        _key: &[u8],
        _control: &StorageReadControl,
    ) -> VersionResult<Option<RecordVersion<SharedRecordValue>>> {
        Err(VersionError::Storage(
            uqa_storage::StorageBackendError::backend("test", std::io::Error::other("read failed")),
        ))
    }

    fn scan(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<ScannedRecord>> {
        let result = self.source.scan(prefix, after, limit, control)?;
        if self.cancel_after_scan {
            control.cancellation().cancel();
        }
        Ok(result)
    }
}

#[test]
fn read_errors_and_cancellation_release_staged_page_allocations() {
    let storage = control();
    let store = MemoryVersionStore::new(storage.memory());
    store
        .commit(&[write(b"key", None, Some(b"value"))], &storage)
        .unwrap();
    let private = PrivateRecordChanges::new(storage.memory());
    let view = MergedRecordSnapshot::new(
        Arc::new(InterruptedSnapshot {
            source: store.snapshot().unwrap(),
            cancel_after_scan: true,
        }),
        private.snapshot().unwrap(),
    );
    let reads = control();
    assert!(matches!(
        view.get(b"key", &reads),
        Err(VersionError::Storage(_))
    ));
    assert!(matches!(
        view.scan(b"", None, 1, &reads),
        Err(VersionError::Cancelled(_))
    ));
    assert_eq!(reads.memory().used(), 0);
    reads.cancellation().reset();
    assert!(view.scan(b"", None, 0, &reads).unwrap().is_empty());
    assert!(!reads.cancellation().is_cancelled());
}

#[test]
fn merge_allocation_failures_release_both_source_pages() {
    let storage = control();
    let store = MemoryVersionStore::new(storage.memory());
    store
        .commit(
            &[write(b"a", None, Some(b"a")), write(b"c", None, Some(b"c"))],
            &storage,
        )
        .unwrap();
    let private = PrivateRecordChanges::new(storage.memory());
    private
        .apply(
            &[write(b"b", None, Some(b"b")), write(b"d", None, Some(b"d"))],
            &storage,
        )
        .unwrap();
    let snapshot = view(&store, &private);
    let mut failed = 0;
    let mut succeeded = 0;
    for allowance in (0..8192).step_by(32) {
        let reads = StorageReadControl::with_limit(allowance);
        match snapshot.scan(b"", None, 4, &reads) {
            Ok(page) => {
                succeeded += 1;
                assert_eq!(page.len(), 4);
            }
            Err(VersionError::Memory(_)) => {
                failed += 1;
            }
            Err(error) => panic!("unexpected read failure: {error}"),
        }
        assert_eq!(reads.memory().used(), 0);
    }
    assert!(failed > 1 && succeeded > 1);
}

proptest! {
    #[test]
    fn merged_pages_match_an_independent_ordered_map(
        base in prop::collection::btree_map(0_u8..64, prop::option::of(any::<u8>()), 0..64),
        overlay in prop::collection::btree_map(0_u8..64, prop::option::of(any::<u8>()), 0..64),
        page_size in 1_usize..12,
    ) {
        let control = control();
        let store = MemoryVersionStore::new(control.memory());
        let writes: Vec<_> = base.iter().map(|(key, value)| write(std::slice::from_ref(key), None, value.as_ref().map(std::slice::from_ref))).collect();
        let revision = store.commit(&writes, &control).unwrap();
        let private = PrivateRecordChanges::new(control.memory());
        let writes: Vec<_> = overlay.iter().map(|(key, value)| write(std::slice::from_ref(key), base.contains_key(key).then_some(revision), value.as_ref().map(std::slice::from_ref))).collect();
        private.apply(&writes, &control).unwrap();
        let snapshot = view(&store, &private);
        let mut expected = base.clone();
        expected.extend(overlay);
        let mut found = BTreeMap::new();
        let mut after = None;
        loop {
            let page = snapshot.scan(b"", after.as_ref().map(std::slice::from_ref), page_size, &control).unwrap();
            for entry in page.iter() {
                let key = entry.key[0];
                prop_assert!(after.is_none_or(|after| key > after));
                let previous = found.insert(key, entry.record.value().map(|value| value[0]));
                prop_assert!(previous.is_none());
                after = Some(key);
            }
            if page.len() < page_size { break; }
        }
        prop_assert_eq!(found, expected);
    }
}
