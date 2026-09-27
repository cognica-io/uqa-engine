//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{KeyValueVectorIndex, VectorIndex};

fn raw(store: &Arc<VersionedKeyValueStore>) -> KeyValueVectorIndex {
    KeyValueVectorIndex::new(store.clone(), TABLE, FIELD, 2)
}

fn mutate(index: &mut KeyValueVectorIndex, operation: usize) {
    match operation {
        0 => index.delete(2).unwrap(),
        1 => index.add(1, vec![0.0, 1.0]).unwrap(),
        _ => index.add(4, vec![1.0, 0.0]).unwrap(),
    }
}

#[test]
fn diskann_population_raw_writer_rejects_first_publication_at_refresh_and_commit() {
    for operation in 0..3 {
        for refresh in [false, true] {
            for explicit_origin in [false, true] {
                let persistence = Persistence::new();
                let publisher = seed(&persistence);
                let built = Built::capture(&publisher);
                let writer = Arc::new(persistence.session(1 << 22));
                writer.begin_transaction().unwrap();
                if explicit_origin {
                    let document = if operation == 0 { 2_u64 } else { 1 };
                    let mut key = b"\0uqa-diskann-canonical-v1\0".to_vec();
                    key.extend_from_slice(&field());
                    key.extend_from_slice(&document.to_be_bytes());
                    writer
                        .put(&key, &writer.get(&key).unwrap().unwrap())
                        .unwrap();
                }
                mutate(&mut raw(&writer), operation);
                built.publish(&publisher, None).unwrap();
                if refresh {
                    assert!(writer
                        .refresh_transaction_snapshot(writer.retention_control().cancellation())
                        .is_err());
                }
                assert!(writer.commit_transaction().is_err());
                writer.rollback_transaction().unwrap();
                assert_eq!(built.counts(&*writer), (3, 0));
                assert_eq!(raw(&writer).count().unwrap(), 3);
            }
        }
    }
}

#[test]
fn diskann_population_first_publication_validates_previously_committed_raw_values() {
    for operation in 0..3 {
        let persistence = Persistence::new();
        let publisher = seed(&persistence);
        let built = Built::capture(&publisher);
        let writer = Arc::new(persistence.session(1 << 22));
        publisher.begin_transaction().unwrap();
        built.publish(&publisher, None).unwrap();
        writer.begin_transaction().unwrap();
        mutate(&mut raw(&writer), operation);
        writer.commit_transaction().unwrap();
        if operation == 0 {
            publisher.commit_transaction().unwrap();
            assert_eq!(built.counts(&*publisher), (1, 0));
        } else {
            assert!(publisher.commit_transaction().is_err());
            publisher.rollback_transaction().unwrap();
            assert!(publisher.get(&built.header).unwrap().is_none());
        }
    }
}

#[test]
fn diskann_population_raw_invalidation_follows_undo_and_later_canonical_replacement() {
    for undo in [false, true] {
        let persistence = Persistence::new();
        let store = seed(&persistence);
        let built = Built::capture(&store);
        store.begin_transaction().unwrap();
        store.savepoint("before_raw").unwrap();
        raw(&store).delete(2).unwrap();
        if undo {
            store.rollback_to_savepoint("before_raw").unwrap();
        } else {
            replace(&store, 2, 3).unwrap();
        }
        built.publish(&store, None).unwrap();
        let expected = if undo { (3, 0) } else { (4, 3) };
        assert_eq!(built.counts(&*store), expected);
        store.commit_transaction().unwrap();
        assert_eq!(built.counts(&*store), expected);
    }
}

#[test]
fn diskann_population_first_publication_censuses_earlier_raw_writes_in_its_own_transaction() {
    for operation in 0..3 {
        let persistence = Persistence::new();
        let store = seed(&persistence);
        let built = Built::capture(&store);
        store.begin_transaction().unwrap();
        mutate(&mut raw(&store), operation);
        if operation == 0 {
            built.publish(&store, None).unwrap();
            assert_eq!(built.counts(&*store), (1, 0));
            persistence
                .session(1 << 22)
                .put(b"peer", b"advance")
                .unwrap();
            store
                .refresh_transaction_snapshot(store.retention_control().cancellation())
                .unwrap();
            assert_eq!(built.counts(&*store), (1, 0));
            store.commit_transaction().unwrap();
            assert_eq!(built.counts(&*store), (1, 0));
        } else {
            assert!(built.publish(&store, None).is_err());
            store.rollback_transaction().unwrap();
            assert!(store.get(&built.header).unwrap().is_none());
        }
    }
}

#[test]
fn diskann_population_invalidation_preserves_ordinary_disjoint_raw_writers() {
    for reverse in [false, true] {
        let persistence = Persistence::new();
        let a = seed(&persistence);
        let b = Arc::new(persistence.session(1 << 22));
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        raw(&a).add(1, vec![0.0, 1.0]).unwrap();
        raw(&b).add(2, vec![1.0, 0.0]).unwrap();
        let (first, second) = if reverse { (&b, &a) } else { (&a, &b) };
        first.commit_transaction().unwrap();
        second
            .refresh_transaction_snapshot(second.retention_control().cancellation())
            .unwrap();
        second.commit_transaction().unwrap();
        assert_eq!(raw(&a).count().unwrap(), 2);
    }
}
