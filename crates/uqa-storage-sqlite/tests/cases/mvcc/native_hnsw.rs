//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW records retain logical visibility, publish with canonical tensors and reopen without rebuilding.

use std::{sync::mpsc, time::Duration};

use super::{open, MODES};
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
