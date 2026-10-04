//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{hnsw_index::HNSWIndex, read_control::StorageReadControl};

const DOCUMENTS: u64 = 20;

fn vector(document: u64, dimensions: usize) -> Vec<f32> {
    let mut vector = vec![0.0; dimensions];
    vector[document as usize % dimensions] = 1.0;
    vector[0] = 0.125;
    vector
}

#[test]
fn physical_hnsw_spill_builds_mutates_rolls_back_and_reopens() {
    lifecycle(false);
}

#[test]
fn native_hnsw_spill_builds_mutates_rolls_back_and_reopens() {
    lifecycle(true);
}

#[test]
fn native_hnsw_loading_spills_a_decoded_graph_beyond_the_session_allowance() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut index = SQLiteHNSWIndex::new(connection.clone(), "docs", "embedding", 64);
    for doc in 0..256 {
        index.add(doc, vec![1.0; 64]).unwrap();
    }
    index.initialize().unwrap();
    crate::SQLiteRecordStore::for_native(&connection, &StorageReadControl::with_limit(16 << 20))
        .unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions {
            retained_bytes: 64 << 10,
        })
        .unwrap();
    let control = connection.retention_control().unwrap();
    assert!(256 * 64 * size_of::<f32>() >= control.memory().limit());
    assert_eq!(index.count().unwrap(), 256);
    let found = index.search_knn(&[1.0; 64], 1).unwrap();
    assert_eq!(found.doc_ids().count(), 1);
    assert!(found.doc_ids().all(|doc| doc < 256));
    let snapshot = index.snapshot().unwrap();
    assert_eq!(snapshot.count().unwrap(), 256);
    drop(snapshot);
    assert_eq!(index.count().unwrap(), 256);
    assert!(control.memory().peak() <= control.memory().limit());
    assert!(!connection.in_transaction());
}

fn lifecycle(native: bool) {
    let dimensions = if native { 4096 } else { 1024 };
    let vector = |document| vector(document, dimensions);
    let directory = tempdir().unwrap();
    let path = directory.path().join("bounded-hnsw.db");
    let params = HNSWIndexParams {
        m: 2,
        ef_construction: 4,
        ef_search: 24,
        rebuild_threshold: 3,
        ..HNSWIndexParams::default()
    };
    let connection = ManagedConnection::open(&path).unwrap();
    Catalog::open(connection.clone()).unwrap();
    let control = bind(&connection, native);
    assert!(DOCUMENTS as usize * dimensions * size_of::<f32>() > control.memory().limit());
    let mut index = SQLiteHNSWIndex::with_params(
        connection.clone(),
        "docs",
        "embedding",
        dimensions as u32,
        params,
    );
    index.physical_control = control.clone();
    let mut reference = HNSWIndex::with_params(dimensions as u32, params).unwrap();
    for document in 1..=DOCUMENTS {
        index.add(document, vector(document)).unwrap();
        reference.add(document, vector(document)).unwrap();
    }
    index.initialize().unwrap();
    let query = vector(17);
    let expected = reference.search_knn(&query, 5).unwrap();
    assert_eq!(index.search_knn(&query, 5).unwrap(), expected);
    let retained = index.snapshot().unwrap();

    connection.begin_transaction().unwrap();
    connection.savepoint("before").unwrap();
    index.add(90, query.clone()).unwrap();
    let discarded = index.snapshot().unwrap();
    connection.rollback_to_savepoint("before").unwrap();
    connection.release_savepoint("before").unwrap();
    assert_eq!(index.search_knn(&query, 5).unwrap(), expected);
    assert_eq!(discarded.count().unwrap(), DOCUMENTS as usize + 1);
    drop(discarded);
    index.delete(1).unwrap();
    connection.rollback_transaction().unwrap();
    assert_eq!(index.count().unwrap(), DOCUMENTS as usize);

    index.add(17, vector(250)).unwrap();
    index.delete(8).unwrap();
    index.delete(9).unwrap();
    reference.add(17, vector(250)).unwrap();
    reference.delete(8).unwrap();
    reference.delete(9).unwrap();
    let changed = reference.search_knn(&query, 5).unwrap();
    assert_eq!(index.search_knn(&query, 5).unwrap(), changed);
    assert_eq!(retained.search_knn(&query, 5).unwrap(), expected);
    assert!(control.memory().peak() <= control.memory().limit());
    drop((index, connection));
    assert!(control.memory().used() > 0);
    assert_eq!(retained.search_knn(&query, 5).unwrap(), expected);
    drop(retained);
    assert_eq!(control.memory().used(), 0);

    let connection = ManagedConnection::open(&path).unwrap();
    let control = bind(&connection, native);
    let mut reopened =
        SQLiteHNSWIndex::open_existing(connection, "docs", "embedding", dimensions as u32, params);
    reopened.physical_control = control.clone();
    assert_eq!(reopened.search_knn(&query, 5).unwrap(), changed);
    assert_eq!(reopened.count().unwrap(), DOCUMENTS as usize - 2);
    drop(reopened);
    assert_eq!(control.memory().used(), 0);
}

fn bind(connection: &ManagedConnection, native: bool) -> StorageReadControl {
    if native {
        connection
            .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions {
                retained_bytes: 256 * 1024,
            })
            .unwrap();
        connection.retention_control().unwrap()
    } else {
        StorageReadControl::with_limit(64 * 1024)
    }
}
