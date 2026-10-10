//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::SQLiteIVFIndex;
use crate::{Catalog, ManagedConnection};
use uqa_storage::{mvcc::VersionedSessionOptions, StorageBackendError, VectorIndex};

#[rstest::rstest]
fn sequential_native_ivf_mutations_retain_state_and_write_only_changed_assignments(
    #[values(false, true)] explicit: bool,
) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut index = SQLiteIVFIndex::with_params(connection.clone(), "items", "vector", 2, 2, 2, 8);
    let mut reference = uqa_storage::IVFIndex::with_params(2, 2, 2, 8);
    for id in 1..=8 {
        let vector = vec![id as f32, 1.0];
        index.add(id, vector.clone()).unwrap();
        reference.add(id, vector).unwrap();
    }
    index.initialize().unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let control = connection.retention_control().unwrap();
    if explicit {
        connection.begin_transaction().unwrap();
    }
    let retained = index.snapshot().unwrap();
    super::state::RESTORED_STATES.set(0);
    super::publication::ASSIGNMENT_WRITES.set(0);
    for id in 9..=40 {
        let vector = vec![id as f32, 1.0];
        index.add(id, vector.clone()).unwrap();
        reference.add(id, vector).unwrap();
    }
    assert_eq!(super::state::RESTORED_STATES.get(), 1);
    assert_eq!(super::publication::ASSIGNMENT_WRITES.get(), 32);
    assert_eq!(
        index.search_knn(&[1.0, 1.0], 8).unwrap(),
        reference.search_knn(&[1.0, 1.0], 8).unwrap()
    );
    assert_eq!(retained.count().unwrap(), 8);
    index
        .add_many(7, vec![vec![-1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    reference
        .add_many(7, vec![vec![-1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    assert_eq!(super::publication::ASSIGNMENT_WRITES.get(), 34);
    index.delete(19).unwrap();
    reference.delete(19).unwrap();
    assert_eq!(
        index.search_knn(&[-1.0, 0.0], 8).unwrap(),
        reference.search_knn(&[-1.0, 0.0], 8).unwrap()
    );
    assert_eq!(super::state::RESTORED_STATES.get(), 1);
    if explicit {
        connection.rollback_transaction().unwrap();
        index.add(90, vec![1.0, 0.0]).unwrap();
        assert_eq!(index.count().unwrap(), 9);
        assert_eq!(super::state::RESTORED_STATES.get(), 2);
    }
    drop((retained, index, connection));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_ivf_probes_and_untrained_search_keep_the_original_reader_control() {
    for threshold in [2, 100] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        Catalog::open(connection.clone()).unwrap();
        let mut index = SQLiteIVFIndex::with_params(
            connection.clone(),
            "docs",
            "embedding",
            2,
            2,
            1,
            threshold,
        );
        index
            .add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
            .unwrap();
        index.add(2, vec![-1.0, 0.0]).unwrap();
        index.initialize().unwrap();
        let expected = index.search_knn(&[1.0, 0.0], 3).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let control = connection.retention_control().unwrap();
        let retained = index.snapshot().unwrap();
        let used = control.memory().used();
        assert_eq!(retained.search_knn(&[1.0, 0.0], 3).unwrap(), expected);
        assert_eq!(control.memory().used(), used);
        let held = control
            .memory()
            .reserve(control.memory().limit() - used)
            .unwrap();
        assert!(matches!(
            retained.search_knn(&[1.0, 0.0], 3),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), control.memory().limit());
        drop(held);
        control.cancellation().cancel();
        assert!(matches!(
            retained.search_knn(&[1.0, 0.0], 3),
            Err(StorageBackendError::Cancelled(_))
        ));
        control.cancellation().reset();
        assert_eq!(retained.search_knn(&[1.0, 0.0], 3).unwrap(), expected);
        assert_eq!(control.memory().used(), used);
        index.add(1, vec![-1.0, 0.0]).unwrap();
        assert_eq!(retained.search_knn(&[1.0, 0.0], 3).unwrap(), expected);
        drop((retained, index, connection));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn native_ivf_reopens_and_mutates_the_reported_corpus_under_the_default_allowance() {
    const DIMENSIONS: usize = 1024;
    const DOCUMENTS: usize = 2338;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ivf.sqlite");
    let connection = ManagedConnection::open(&path).unwrap();
    Catalog::open(connection.clone()).unwrap();
    // Seed a valid existing training generation without making fixture setup run k-means.
    connection
        .with_mut(|connection| {
            let transaction = connection.savepoint()?;
            transaction.execute(
                "INSERT INTO _ivf_indexes VALUES
                 ('docs', 'embedding', 1024, 100, 10, 256, 'trained', 2338, 0, 2338)",
                [],
            )?;
            let mut vector = vec![0.0_f32; DIMENSIONS];
            for coordinate in 0..100 {
                vector[coordinate] = 1.0;
                let bytes = vector
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect::<Vec<_>>();
                transaction.execute(
                    "INSERT INTO _ivf_centroids VALUES ('docs', 'embedding', ?1, ?2)",
                    (coordinate as i64, bytes),
                )?;
                vector[coordinate] = 0.0;
            }
            for document in 1..=DOCUMENTS {
                vector[document % 100] = 1.0;
                let bytes = vector
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect::<Vec<_>>();
                transaction.execute(
                    "INSERT INTO _vectors VALUES ('docs', 'embedding', ?1, 0, ?2)",
                    (document as i64, bytes),
                )?;
                transaction.execute(
                    "INSERT INTO _ivf_assignments VALUES ('docs', 'embedding', ?1, 0, ?2)",
                    (document as i64, (document % 100) as i64),
                )?;
                vector[document % 100] = 0.0;
            }
            transaction.commit()?;
            Ok(())
        })
        .unwrap();
    drop(connection);

    for reopen in 0..2 {
        let connection = ManagedConnection::open(&path).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        Catalog::open(connection.clone()).unwrap();
        let control = connection.retention_control().unwrap();
        assert_eq!(control.memory().limit(), 64 * 1024 * 1024);
        let mut index =
            SQLiteIVFIndex::new(connection.clone(), "docs", "embedding", DIMENSIONS as u32);
        let retained = index.snapshot().unwrap();
        let document = DOCUMENTS as u64 + 1;
        let mut query = vec![0.0; DIMENSIONS];
        query[100 + reopen] = 1.0;
        index.add(document, query.clone()).unwrap();
        assert_eq!(index.count().unwrap(), DOCUMENTS + 1);
        assert_eq!(
            index
                .search_knn(&query, 1)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            vec![document]
        );
        assert_eq!(retained.count().unwrap(), DOCUMENTS);
        assert!(!retained.contains_document(document).unwrap());
        index.add(document, vec![1.0; DIMENSIONS]).unwrap();
        assert_eq!(index.count().unwrap(), DOCUMENTS + 1);
        index.delete(document).unwrap();
        assert_eq!(index.count().unwrap(), DOCUMENTS);
        assert!(!index.contains_document(document).unwrap());
        assert!(control.memory().peak() <= control.memory().limit());
        drop((retained, index, connection));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn native_ivf_spills_training_and_preserves_undo_retained_readers_and_reopen() {
    const DIMENSIONS: usize = 2048;
    const DOCUMENTS: u64 = 40;
    let vector = |document: u64| {
        let mut vector = vec![0.0; DIMENSIONS];
        vector[document as usize % DIMENSIONS] = 1.0;
        vector[0] = 0.125;
        vector
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded-ivf.db");
    let connection = ManagedConnection::open(&path).unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .with_mut(|connection| {
            let transaction = connection.savepoint()?;
            for document in 1..=DOCUMENTS {
                transaction.execute(
                    "INSERT INTO _vectors VALUES ('docs', 'embedding', ?1, 0, ?2)",
                    (
                        document as i64,
                        crate::vector_index::vector_to_blob(&vector(document))?,
                    ),
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 256 * 1024,
        })
        .unwrap();
    let control = connection.retention_control().unwrap();
    assert!(DOCUMENTS as usize * DIMENSIONS * size_of::<f32>() > control.memory().limit());
    let mut index = SQLiteIVFIndex::with_params(
        connection.clone(),
        "docs",
        "embedding",
        DIMENSIONS as u32,
        2,
        2,
        4,
    );
    let mut reference = uqa_storage::IVFIndex::with_params(DIMENSIONS as u32, 2, 2, 4);
    for document in 1..=DOCUMENTS {
        reference.add(document, vector(document)).unwrap();
    }
    reference.train().unwrap();
    index.initialize().unwrap();
    let query = vector(17);
    let expected = reference.search_knn(&query, 5).unwrap();
    assert_eq!(index.search_knn(&query, 5).unwrap(), expected);
    let retained = index.snapshot().unwrap();
    connection.begin_transaction().unwrap();
    connection.savepoint("before").unwrap();
    index
        .add_many(90, vec![query.clone(), vector(250)])
        .unwrap();
    let discarded = index.snapshot().unwrap();
    connection.rollback_to_savepoint("before").unwrap();
    connection.release_savepoint("before").unwrap();
    assert_eq!(index.search_knn(&query, 5).unwrap(), expected);
    assert_eq!(discarded.count().unwrap(), DOCUMENTS as usize + 2);
    assert!(discarded.contains_document(90).unwrap());
    drop(discarded);
    index.delete(1).unwrap();
    connection.rollback_transaction().unwrap();
    assert_eq!(index.count().unwrap(), DOCUMENTS as usize);
    index.add(17, vector(250)).unwrap();
    reference.add(17, vector(250)).unwrap();
    for document in 1..=9 {
        index.delete(document).unwrap();
        reference.delete(document).unwrap();
        if reference.state() == uqa_storage::ivf_index::IVFState::Stale {
            reference.train().unwrap();
        }
    }
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
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 256 * 1024,
        })
        .unwrap();
    let control = connection.retention_control().unwrap();
    let index =
        SQLiteIVFIndex::with_params(connection, "docs", "embedding", DIMENSIONS as u32, 2, 2, 4);
    assert_eq!(index.search_knn(&query, 5).unwrap(), changed);
    assert_eq!(index.count().unwrap(), DOCUMENTS as usize - 9);
    drop(index);
    assert_eq!(control.memory().used(), 0);
}
