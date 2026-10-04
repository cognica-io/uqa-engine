//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Catalog, ManagedConnection};
use uqa_storage::{mvcc::VersionedSessionOptions, read_control::StorageReadControl};

#[test]
fn exact_native_scoring_streams_the_corpus_under_a_bounded_read_allowance() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut index = SQLiteVectorIndex::new(connection.clone(), "docs", "embedding", 128);
    connection.begin_transaction().unwrap();
    for id in 1..=1024 {
        let mut vector = vec![0.0; 128];
        vector[usize::try_from(id % 128).unwrap()] = 1.0;
        index.add(id, vector).unwrap();
    }
    index
        .add_many(
            1,
            vec![vec![0.0; 128], {
                let mut vector = vec![0.0; 128];
                vector[0] = 1.0;
                vector
            }],
        )
        .unwrap();
    connection.commit_transaction().unwrap();
    let mut captured = Arc::try_unwrap(connection.native_snapshot().unwrap().unwrap())
        .ok()
        .unwrap();
    captured.control = StorageReadControl::with_limit(65536);
    let control = captured.control.clone();
    index.retained = Some(Arc::new(captured));
    let mut query = vec![0.0; 128];
    query[0] = 1.0;
    let actual = index.search_knn(&query, 3).unwrap();
    assert_eq!(
        actual
            .entries()
            .iter()
            .map(|entry| entry.doc_id)
            .collect::<Vec<_>>(),
        [1, 128, 256]
    );
    for entry in actual.entries() {
        assert_eq!(entry.payload.score, 1.0);
    }
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() < 65536);
    let actual = index.search_threshold(&query, 0.5).unwrap();
    assert_eq!(
        actual
            .entries()
            .iter()
            .map(|entry| entry.doc_id)
            .collect::<Vec<_>>(),
        [1, 128, 256, 384, 512, 640, 768, 896, 1024]
    );
    assert_eq!(control.memory().used(), 0);
}
