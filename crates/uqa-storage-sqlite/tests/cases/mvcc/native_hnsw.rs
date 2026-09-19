//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW records retain logical visibility, publish with canonical tensors and reopen without rebuilding.

use std::{sync::mpsc, time::Duration};

use super::{
    native_vectors::{generation, IndexKind},
    open, MODES,
};
use uqa_storage::{
    mvcc::VersionedSessionOptions,
    read_control::StorageReadControl,
    vector_index::{HNSWIndexParams, VectorIndex},
};
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteHNSWIndex, SQLiteRecordStore};

const X: [f32; 3] = [1.0, 0.0, 0.0];
const Y: [f32; 3] = [0.0, 1.0, 0.0];
const Z: [f32; 3] = [0.0, 0.0, 1.0];

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

fn nearest(index: &dyn VectorIndex, query: &[f32]) -> Vec<u64> {
    index.search_knn(query, 1).unwrap().doc_ids().collect()
}

#[test]
fn independent_native_hnsw_documents_merge_shared_nodes_like_serial_execution() {
    for mode in MODES {
        for reverse in [false, true] {
            for seed in [0, 8] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("shared-hnsw.db");
                let connection = open(mode, &path);
                let serial = ManagedConnection::open_in_memory().unwrap();
                for source in [&connection, &serial] {
                    Catalog::open(source.clone()).unwrap();
                    let mut index = SQLiteHNSWIndex::new(source.clone(), "docs", "embedding", 3);
                    for document in 1..=seed {
                        index
                            .add(document, vec![1.0, document as f32, 0.0])
                            .unwrap();
                    }
                    index.initialize().unwrap();
                    bind(source);
                }
                let other = open(mode, &path);
                bind(&other);
                let mut left = SQLiteHNSWIndex::new(connection.clone(), "docs", "embedding", 3);
                let mut right = SQLiteHNSWIndex::new(other.clone(), "docs", "embedding", 3);
                let mut reference = SQLiteHNSWIndex::new(serial.clone(), "docs", "embedding", 3);
                let baseline = left.snapshot().unwrap();
                connection.begin_transaction().unwrap();
                other.begin_transaction().unwrap();
                left.add_many(101, vec![X.to_vec(), Y.to_vec()]).unwrap();
                right.add(102, Z.to_vec()).unwrap();
                let private = left.snapshot().unwrap();
                if reverse {
                    connection.commit_transaction().unwrap();
                    assert!(other.in_transaction());
                    other.commit_transaction().unwrap();
                    reference
                        .add_many(101, vec![X.to_vec(), Y.to_vec()])
                        .unwrap();
                    reference.add(102, Z.to_vec()).unwrap();
                } else {
                    other.commit_transaction().unwrap();
                    assert!(connection.in_transaction());
                    connection.commit_transaction().unwrap();
                    reference.add(102, Z.to_vec()).unwrap();
                    reference
                        .add_many(101, vec![X.to_vec(), Y.to_vec()])
                        .unwrap();
                }
                assert_eq!(
                    generation(&connection, IndexKind::Hnsw, "docs", "embedding"),
                    generation(&serial, IndexKind::Hnsw, "docs", "embedding")
                );
                assert_eq!(left.count().unwrap(), seed as usize + 3);
                assert_eq!(right.count().unwrap(), seed as usize + 3);
                assert_eq!(baseline.count().unwrap(), seed as usize);
                assert_eq!(private.count().unwrap(), seed as usize + 2);
            }
        }
    }
}

#[test]
fn native_hnsw_ordered_merges_compact_after_savepoint_rollback_and_publication_retry() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ordered-hnsw.db");
        let a = open(mode, &path);
        let serial = ManagedConnection::open_in_memory().unwrap();
        let params = HNSWIndexParams {
            rebuild_threshold: 2,
            ..HNSWIndexParams::default()
        };
        for source in [&a, &serial] {
            Catalog::open(source.clone()).unwrap();
            let mut index =
                SQLiteHNSWIndex::with_params(source.clone(), "docs", "embedding", 3, params);
            for document in 1..=8 {
                index
                    .add(document, vec![1.0, document as f32, 0.5])
                    .unwrap();
            }
            index.initialize().unwrap();
            bind(source);
        }
        let b = open(mode, &path);
        bind(&b);
        let mut left = SQLiteHNSWIndex::with_params(a.clone(), "docs", "embedding", 3, params);
        let mut right = SQLiteHNSWIndex::with_params(b.clone(), "docs", "embedding", 3, params);
        let mut reference =
            SQLiteHNSWIndex::with_params(serial.clone(), "docs", "embedding", 3, params);
        let baseline = left.snapshot().unwrap();
        a.begin_transaction().unwrap();
        a.savepoint("discard").unwrap();
        left.add(99, X.to_vec()).unwrap();
        let discarded = left.snapshot().unwrap();
        a.rollback_to_savepoint("discard").unwrap();
        a.release_savepoint("discard").unwrap();
        left.delete(1).unwrap();
        left.delete(2).unwrap();
        for vectors in [
            vec![X.to_vec(), Y.to_vec()],
            vec![],
            vec![Y.to_vec(), Z.to_vec()],
        ] {
            left.add_many(11, vectors).unwrap();
        }
        assert!(left.add_many(11, vec![vec![f32::NAN; 3]]).is_err());
        let private = left.snapshot().unwrap();
        right.add(12, Z.to_vec()).unwrap();
        let before = generation(&b, IndexKind::Hnsw, "docs", "embedding");
        b.with_physical(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER fail_hnsw_merge BEFORE INSERT ON _hnsw_edges BEGIN SELECT RAISE(ABORT, 'injected HNSW merge failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(a.commit_transaction().is_err());
        assert_eq!(generation(&b, IndexKind::Hnsw, "docs", "embedding"), before);
        b.with_physical(|sqlite| {
            sqlite.execute_batch("DROP TRIGGER fail_hnsw_merge")?;
            Ok(())
        })
        .unwrap();
        right.add(13, X.to_vec()).unwrap();
        a.commit_transaction().unwrap();
        reference.add(12, Z.to_vec()).unwrap();
        reference.add(13, X.to_vec()).unwrap();
        reference.delete(1).unwrap();
        reference.delete(2).unwrap();
        for vectors in [
            vec![X.to_vec(), Y.to_vec()],
            vec![],
            vec![Y.to_vec(), Z.to_vec()],
        ] {
            reference.add_many(11, vectors).unwrap();
        }
        let expected = generation(&serial, IndexKind::Hnsw, "docs", "embedding");
        assert_eq!(
            generation(&a, IndexKind::Hnsw, "docs", "embedding"),
            expected
        );
        assert_eq!(baseline.count().unwrap(), 8);
        assert_eq!(discarded.count().unwrap(), 9);
        assert_eq!(private.count().unwrap(), 8);
        assert_eq!(left.count().unwrap(), 10);
        assert_eq!(right.count().unwrap(), 10);
        for query in [&X, &Y, &Z] {
            let actual = left.search_knn(query, 20).unwrap();
            let serial = reference.search_knn(query, 20).unwrap();
            assert_eq!(
                actual.doc_ids().collect::<Vec<_>>(),
                serial.doc_ids().collect::<Vec<_>>()
            );
        }
        drop((left, right, baseline, private, discarded, a, b));
        let reopened = open(mode, &path);
        bind(&reopened);
        assert_eq!(
            generation(&reopened, IndexKind::Hnsw, "docs", "embedding"),
            expected
        );
        let restored = SQLiteHNSWIndex::open_existing(reopened, "docs", "embedding", 3, params);
        assert_eq!(restored.count().unwrap(), 10);
    }
}

#[test]
fn independent_hnsw_fields_publish_while_another_graph_remains_private() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("hnsw.db");
            let connection = open(mode, &path);
            Catalog::open(connection.clone()).unwrap();
            let mut a = SQLiteHNSWIndex::new(connection.clone(), "docs", "first", 3);
            let mut b = SQLiteHNSWIndex::new(connection.clone(), "docs", "second", 3);
            a.add(1, X.to_vec()).unwrap();
            b.add(1, Y.to_vec()).unwrap();
            a.initialize().unwrap();
            b.initialize().unwrap();
            bind(&connection);
            connection.begin_transaction().unwrap();
            a.add(2, Z.to_vec()).unwrap();
            connection.savepoint("keep").unwrap();
            let private = a.snapshot().unwrap();
            a.add(3, Y.to_vec()).unwrap();
            let other_path = path.clone();
            let (sent, received) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let other = open(mode, &other_path);
                bind(&other);
                let mut index = SQLiteHNSWIndex::open_existing(
                    other,
                    "docs",
                    "second",
                    3,
                    HNSWIndexParams::default(),
                );
                index.add(2, X.to_vec()).unwrap();
                sent.send(nearest(&index, &X)).unwrap();
            });
            assert_eq!(
                received.recv_timeout(Duration::from_secs(20)).unwrap(),
                vec![2]
            );
            writer.join().unwrap();
            assert_eq!(b.count().unwrap(), 1);
            match ending {
                "commit" => connection.commit_transaction().unwrap(),
                "rollback" => connection.rollback_transaction().unwrap(),
                _ => {
                    connection.rollback_to_savepoint("keep").unwrap();
                    connection.commit_transaction().unwrap();
                }
            }
            let expected = match ending {
                "commit" => 3,
                "rollback" => 1,
                _ => 2,
            };
            assert_eq!(a.count().unwrap(), expected);
            assert_eq!(b.count().unwrap(), 2);
            assert_eq!(nearest(&b, &X), vec![2]);
            assert_eq!(nearest(&*private, &Z), vec![2]);
            drop((a, b, private, connection));
            let reopened = open(mode, &path);
            bind(&reopened);
            let a = SQLiteHNSWIndex::open_existing(
                reopened.clone(),
                "docs",
                "first",
                3,
                HNSWIndexParams::default(),
            );
            let b = SQLiteHNSWIndex::open_existing(
                reopened,
                "docs",
                "second",
                3,
                HNSWIndexParams::default(),
            );
            assert_eq!(a.count().unwrap(), expected);
            assert_eq!(nearest(&a, &X), vec![1]);
            assert_eq!(nearest(&b, &X), vec![2]);
        }
    }
}

#[test]
fn hnsw_publication_failure_keeps_one_evaluated_generation_for_retry_and_reopen() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("publication.db");
        let connection = open(mode, &path);
        Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        let mut index = SQLiteHNSWIndex::new(connection.clone(), "new\0日本語", "embedding", 3);
        let empty = index.snapshot().unwrap();
        index.initialize().unwrap();
        index.add(1, X.to_vec()).unwrap();
        index.add_many(2, vec![Y.to_vec(), Z.to_vec()]).unwrap();
        let old = index.snapshot().unwrap();
        connection.begin_transaction().unwrap();
        index.add_many(1, vec![Y.to_vec(), Z.to_vec()]).unwrap();
        let private = index.snapshot().unwrap();
        let observer = connection.new_session();
        observer.with_physical(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER fail_hnsw_publication BEFORE INSERT ON _hnsw_nodes BEGIN SELECT RAISE(ABORT, 'injected HNSW publication failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(connection.commit_transaction().is_err());
        assert!(connection.in_transaction());
        assert!(index.add(3, X.to_vec()).is_err());
        let visible = SQLiteHNSWIndex::new(observer.clone(), "new\0日本語", "embedding", 3);
        assert_eq!(visible.count().unwrap(), 3);
        assert_eq!(nearest(&visible, &X), vec![1]);
        observer
            .with_physical(|sqlite| {
                sqlite.execute_batch("DROP TRIGGER fail_hnsw_publication")?;
                Ok(())
            })
            .unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(visible.count().unwrap(), 4);
        assert!(visible.search_threshold(&X, 0.99).unwrap().is_empty());
        assert_eq!(empty.count().unwrap(), 0);
        assert_eq!(nearest(&*old, &X), vec![1]);
        assert_eq!(private.count().unwrap(), 4);
        let revision = connection
            .with_physical(|sqlite| {
                Ok(
                    sqlite.query_row("SELECT revision FROM _hnsw_indexes", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(revision, 4);
        drop((index, visible, empty, old, private, observer, connection));
        let reopened = open(mode, &path);
        bind(&reopened);
        let restored = SQLiteHNSWIndex::open_existing(
            reopened.clone(),
            "new\0日本語",
            "embedding",
            3,
            HNSWIndexParams::default(),
        );
        assert_eq!(restored.count().unwrap(), 4);
        assert_eq!(nearest(&restored, &Y), vec![1]);
        assert_eq!(
            reopened
                .with_physical(|sqlite| Ok(sqlite.query_row(
                    "SELECT revision FROM _hnsw_indexes",
                    [],
                    |row| row.get::<_, i64>(0)
                )?))
                .unwrap(),
            revision
        );
        let retained = restored.snapshot().unwrap();
        SQLiteHNSWIndex::drop_metadata(&reopened, "new\0日本語", "embedding").unwrap();
        assert!(restored.search_knn(&Y, 1).is_err());
        assert_eq!(nearest(&*retained, &Y), vec![1]);
    }
}

#[test]
fn native_hnsw_loading_checks_the_aggregate_decoded_graph_allowance() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut index = SQLiteHNSWIndex::new(connection.clone(), "docs", "embedding", 64);
    for doc in 0..256 {
        index.add(doc, vec![1.0; 64]).unwrap();
    }
    index.initialize().unwrap();
    SQLiteRecordStore::for_native(&connection, &StorageReadControl::with_limit(16 << 20)).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 64 << 10,
        })
        .unwrap();
    assert_eq!(index.count().unwrap(), 256);
    assert!(matches!(
        index.search_knn(&[1.0; 64], 1).unwrap_err(),
        uqa_storage::StorageBackendError::Memory(_)
    ));
    assert_eq!(index.count().unwrap(), 256);
    assert!(index.snapshot().is_err());
    assert!(!connection.in_transaction());
}
