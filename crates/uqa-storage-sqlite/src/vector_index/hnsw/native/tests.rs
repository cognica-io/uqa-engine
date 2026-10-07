//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Catalog, ManagedConnection, SQLiteVectorIndex};
use uqa_storage::{
    hnsw_index::HNSWIndex, mvcc::VersionedSessionOptions, vector_index::HNSWIndexParams,
};
use uqa_storage::{ReadOnlySnapshot, VectorIndex};

mod retention;

#[rstest::rstest]
fn sequential_native_hnsw_mutations_do_not_restore_the_entire_graph(
    #[values(false, true)] explicit: bool,
) {
    let (connection, mut index) = fixture();
    let mut expected = HNSWIndex::new(2);
    expected.add(1, vec![1.0, 0.0]).unwrap();
    expected.add(2, vec![0.0, 1.0]).unwrap();
    if explicit {
        connection.begin_transaction().unwrap();
    }
    let retained = index.snapshot().unwrap();
    loading::RESTORED_GRAPHS.set(0);
    for id in 3..=34 {
        index.add(id, vec![id as f32, 1.0]).unwrap();
        expected.add(id, vec![id as f32, 1.0]).unwrap();
        assert_eq!(
            index.search_knn(&[id as f32, 1.0], 8).unwrap(),
            expected.search_knn(&[id as f32, 1.0], 8).unwrap()
        );
    }
    index.add(7, vec![-1.0, 0.0]).unwrap();
    expected.add(7, vec![-1.0, 0.0]).unwrap();
    index.delete(19).unwrap();
    expected.delete(19).unwrap();
    assert_eq!(
        index.search_knn(&[-1.0, 0.0], 8).unwrap(),
        expected.search_knn(&[-1.0, 0.0], 8).unwrap()
    );
    assert_eq!(
        loading::RESTORED_GRAPHS.get(),
        0,
        "own mutations must retain their evaluated graph"
    );
    assert_eq!(retained.count().unwrap(), 2);
    index.clear().unwrap();
    assert_eq!(graph(&index).count().unwrap(), 0);
    assert_eq!(loading::RESTORED_GRAPHS.get(), 0);
    if explicit {
        connection.rollback_transaction().unwrap();
        assert_eq!(graph(&index).count().unwrap(), 2);
        assert_eq!(
            loading::RESTORED_GRAPHS.get(),
            1,
            "rollback must restore the original graph"
        );
    }
}

fn fixture() -> (ManagedConnection, SQLiteHNSWIndex) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut index = SQLiteHNSWIndex::new(connection.clone(), "docs", "embedding", 2);
    index.add(1, vec![1.0, 0.0]).unwrap();
    index.add(2, vec![0.0, 1.0]).unwrap();
    index.initialize().unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    (connection, index)
}

fn graph(index: &SQLiteHNSWIndex) -> ReadOnlySnapshot<HNSWIndex> {
    index
        .persistent
        .read_native(|read| index.cached_native_graph(read))
        .unwrap()
        .unwrap()
        .unwrap()
}

fn nearest(index: &dyn VectorIndex, query: &[f32]) -> Vec<u64> {
    index.search_knn(query, 1).unwrap().doc_ids().collect()
}

#[test]
fn private_hnsw_cache_identities_distinguish_equal_revision_rollback_branches() {
    let (connection, mut index) = fixture();
    let stable = graph(&index);
    assert!(std::ptr::eq(&raw const *stable, &raw const *graph(&index)));
    let mut unrelated = SQLiteVectorIndex::new(connection.new_session(), "other", "embedding", 2);
    unrelated.add(1, vec![1.0, 0.0]).unwrap();
    assert!(std::ptr::eq(&raw const *stable, &raw const *graph(&index)));
    connection.begin_transaction().unwrap();
    connection.savepoint("before").unwrap();
    index.add(3, vec![-1.0, 0.0]).unwrap();
    let discarded = index.snapshot().unwrap();
    let revision = index.persisted_revision().unwrap();
    assert_eq!(nearest(&index, &[-1.0, 0.0]), vec![3]);
    connection.rollback_to_savepoint("before").unwrap();
    let mut other_handle = SQLiteHNSWIndex::new(connection.clone(), "docs", "embedding", 2);
    other_handle.add(4, vec![-1.0, 0.0]).unwrap();
    assert_eq!(other_handle.persisted_revision().unwrap(), revision);
    assert_eq!(nearest(&index, &[-1.0, 0.0]), vec![4]);
    assert_eq!(nearest(&*discarded, &[-1.0, 0.0]), vec![3]);
    connection.rollback_transaction().unwrap();
    let old = index.snapshot().unwrap();
    other_handle.add(5, vec![-1.0, 0.0]).unwrap();
    assert_eq!(other_handle.persisted_revision().unwrap(), revision);
    assert_eq!(nearest(&index, &[-1.0, 0.0]), vec![5]);
    assert_eq!(old.count().unwrap(), 2);
    assert_eq!(discarded.count().unwrap(), 3);
}

#[test]
fn native_warm_graphs_reject_private_and_committed_canonical_drift() {
    for private in [false, true] {
        let (connection, index) = fixture();
        let retained = index.snapshot().unwrap();
        assert_eq!(nearest(&index, &[1.0, 0.0]), vec![1]);
        if private {
            connection.begin_transaction().unwrap();
        }
        let mut raw = SQLiteVectorIndex::new(connection.clone(), "docs", "embedding", 2);
        raw.add(1, vec![-1.0, 0.0]).unwrap();
        assert!(index
            .search_knn(&[1.0, 0.0], 1)
            .unwrap_err()
            .to_string()
            .contains("differs from its live graph node"));
        assert_eq!(nearest(&*retained, &[1.0, 0.0]), vec![1]);
        if private {
            connection.rollback_transaction().unwrap();
            assert_eq!(nearest(&index, &[1.0, 0.0]), vec![1]);
        }
    }
}

#[test]
fn native_hnsw_catalog_validation_checks_persisted_parameters_and_missing_headers() {
    let (connection, _) = fixture();
    let different = SQLiteHNSWIndex::open_existing(
        connection.clone(),
        "docs",
        "embedding",
        2,
        HNSWIndexParams {
            seed: 77,
            ..HNSWIndexParams::default()
        },
    );
    assert!(different.validate_existing().is_err());
    assert!(different.search_knn(&[1.0, 0.0], 1).is_err());
    let mut required = SQLiteHNSWIndex::open_existing(
        connection.clone(),
        "docs",
        "embedding",
        2,
        HNSWIndexParams::default(),
    );
    required.validate_existing().unwrap();
    let retained = required.snapshot().unwrap();
    SQLiteHNSWIndex::drop_metadata(&connection, "docs", "embedding").unwrap();
    assert!(required.validate_existing().is_err());
    assert!(required.search_knn(&[1.0, 0.0], 1).is_err());
    assert!(required.snapshot().is_err());
    assert!(required.add(3, vec![1.0, 0.0]).is_err());
    assert!(required.initialize().is_err());
    assert_eq!(required.count().unwrap(), 2);
    assert_eq!(nearest(&*retained, &[1.0, 0.0]), vec![1]);
}

#[test]
fn native_hnsw_revision_exhaustion_does_not_stage_the_canonical_replacement() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut index = SQLiteHNSWIndex::new(connection.clone(), "docs", "embedding", 2);
    index.add(1, vec![1.0, 0.0]).unwrap();
    index.initialize().unwrap();
    connection
        .with(|sqlite| {
            sqlite.execute("UPDATE _hnsw_indexes SET revision = ?1", [i64::MAX])?;
            Ok(())
        })
        .unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    connection.begin_transaction().unwrap();
    let mut other = SQLiteVectorIndex::new(connection.clone(), "other", "embedding", 2);
    other.add(2, vec![0.0, 1.0]).unwrap();
    assert!(index
        .add(1, vec![-1.0, 0.0])
        .unwrap_err()
        .to_string()
        .contains("revision"));
    assert_eq!(nearest(&index, &[1.0, 0.0]), vec![1]);
    connection.commit_transaction().unwrap();
    assert_eq!(index.persisted_revision().unwrap(), Some(i64::MAX as u64));
    assert_eq!(
        index
            .search_threshold(&[1.0, 0.0], 0.99)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(other.count().unwrap(), 1);
}

#[test]
fn intervening_native_recreation_rejects_the_obsolete_candidate_without_replaying_it() {
    let (connection, index) = fixture();
    let other = connection.new_session();
    let mut calls = 0;
    let result = index.write_native_graph(|read, batch| {
        index.mutate_native(
            read,
            batch,
            uqa_storage::hnsw_index::HNSWMutation::Replace {
                document: 3,
                vectors: &[vec![0.0, -1.0]],
            },
            |read, batch| {
                calls += 1;
                other.begin_transaction().unwrap();
                SQLiteHNSWIndex::drop_metadata(&other, "docs", "embedding").unwrap();
                let mut raw = SQLiteVectorIndex::new(other.clone(), "docs", "embedding", 2);
                raw.clear().unwrap();
                raw.add(4, vec![-1.0, 0.0]).unwrap();
                let mut rebuilt = SQLiteHNSWIndex::new(other.clone(), "docs", "embedding", 2);
                rebuilt.initialize().unwrap();
                assert_eq!(rebuilt.persisted_revision().unwrap(), Some(1));
                other.commit_transaction().unwrap();
                read.replace(
                    batch,
                    3,
                    &[(0, crate::vector_index::vector_to_blob(&[0.0, -1.0])?)],
                )
            },
        )
    });
    let error: uqa_storage::StorageBackendError = result.unwrap_err().into();
    let uqa_storage::StorageBackendError::Backend { source, .. } = error else {
        panic!("expected the original field lifetime conflict");
    };
    assert!(
        matches!(
            source.downcast_ref::<uqa_storage::mvcc::VersionError>(),
            Some(uqa_storage::mvcc::VersionError::ReadConflict { .. })
        ),
        "{source:?}"
    );
    assert_eq!(calls, 1);
    assert!(connection.in_transaction());
    let observer = SQLiteHNSWIndex::new(other, "docs", "embedding", 2);
    assert_eq!(observer.count().unwrap(), 1);
    assert_eq!(nearest(&observer, &[-1.0, 0.0]), vec![4]);
    connection.rollback_transaction().unwrap();
    assert_eq!(index.count().unwrap(), 1);
    assert_eq!(nearest(&index, &[-1.0, 0.0]), vec![4]);
}

#[test]
fn native_candidate_cannot_hide_an_intervening_committed_graph_mutation() {
    let (connection, index) = fixture();
    let other = connection.new_session();
    let mut peer = SQLiteHNSWIndex::new(other, "docs", "embedding", 2);
    let retained = index.snapshot().unwrap();
    let mut calls = 0;
    assert!(index
        .write_native_graph(|read, batch| {
            index.mutate_native(
                read,
                batch,
                HNSWMutation::Replace {
                    document: 3,
                    vectors: &[vec![-1.0, 0.0]],
                },
                |read, batch| {
                    calls += 1;
                    peer.add(4, vec![0.0, -1.0]).unwrap();
                    read.replace(
                        batch,
                        3,
                        &[(0, crate::vector_index::vector_to_blob(&[-1.0, 0.0])?)],
                    )
                },
            )
        })
        .unwrap());
    assert_eq!(calls, 1);
    assert_eq!(graph(&index).count().unwrap(), 4);
    assert_eq!(nearest(&index, &[-1.0, 0.0]), vec![3]);
    assert_eq!(nearest(&index, &[0.0, -1.0]), vec![4]);
    assert_eq!(retained.count().unwrap(), 2);
}
