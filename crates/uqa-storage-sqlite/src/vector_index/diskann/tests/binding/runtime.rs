//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{canonical, capture, identity::Resolver, open, row, setup, FIELD, TABLE};
use crate::{ManagedConnection, SQLiteVectorIndex};
use std::sync::Arc;
use uqa_storage::{
    diskann_index::{
        build::DiskANNTemporaryBudget, pages::DiskANNPageSource, PersistentDiskANNIndex,
    },
    key_value::conformance::diskann_runtime_fixture_options,
    read_control::StorageReadControl,
    VectorIndex,
};

fn runtime(
    connection: &ManagedConnection,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
) -> impl VectorIndex {
    let options = diskann_runtime_fixture_options(2).unwrap();
    let handle = canonical(connection, TABLE, FIELD, 2)
        .bind(row().relation, Arc::new(Resolver), options.read, control)
        .unwrap();
    PersistentDiskANNIndex::new(handle, options, temporary).unwrap()
}

fn scores(index: &dyn VectorIndex, expected: &[(u64, f64)]) {
    assert_eq!(
        index
            .search_knn(&[1.0, 0.0], 10)
            .unwrap()
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|&(document, score)| (document, score.to_bits()))
            .collect::<Vec<_>>()
    );
}

fn create(
    connection: &ManagedConnection,
    temporary: &DiskANNTemporaryBudget,
    control: &StorageReadControl,
) {
    let catalog = setup(connection);
    let mut definition = row();
    definition.definition_json = Some(serde_json::to_string(&[82; 16]).unwrap());
    catalog.save_catalog_index_row(&definition).unwrap();
    let options = diskann_runtime_fixture_options(2).unwrap();
    let canonical = canonical(connection, TABLE, FIELD, 2);
    let mut raw = SQLiteVectorIndex::new(connection.clone(), TABLE, FIELD, 2);
    raw.add(1, vec![1.0, -0.0]).unwrap();
    raw.add_many(2, vec![vec![0.0, 1.0], vec![1.0, 0.0]])
        .unwrap();
    raw.add(3, vec![-1.0, 0.0]).unwrap();
    assert!(canonical
        .create_index(&definition.relation, &Resolver, options, temporary, control)
        .is_err());
    connection.begin_transaction().unwrap();
    catalog.set_metadata("outer-runtime-write", "kept").unwrap();
    catalog.save_catalog_index_row(&definition).unwrap();
    let mut bad = options;
    bad.generation.max_record_bytes = 1;
    assert!(canonical
        .create_index(&definition.relation, &Resolver, bad, temporary, control)
        .is_err());
    let mut unreadable = options;
    unreadable.read.resident_bytes = 1;
    assert!(canonical
        .create_index(
            &definition.relation,
            &Resolver,
            unreadable,
            temporary,
            control
        )
        .is_err());
    assert_eq!(raw.count().unwrap(), 4);
    assert!(canonical
        .retain(control)
        .unwrap()
        .origin(1, control)
        .is_err());
    assert_eq!(
        catalog
            .get_metadata("outer-runtime-write")
            .unwrap()
            .as_deref(),
        Some("kept")
    );
    canonical
        .create_index(&definition.relation, &Resolver, options, temporary, control)
        .unwrap();
    assert!(connection.in_transaction());
}

#[test]
fn native_diskann_runtime_adoption_conflicts_with_unstamped_insertions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("adoption.db");
    let connection = open(&path, 0);
    let catalog = setup(&connection);
    let mut definition = row();
    definition.definition_json = Some(serde_json::to_string(&[82; 16]).unwrap());
    catalog.save_catalog_index_row(&definition).unwrap();
    let peer = open(&path, 0);
    let control = StorageReadControl::with_limit(1 << 21);
    let options = diskann_runtime_fixture_options(2).unwrap();
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let mut raw = SQLiteVectorIndex::new(connection.clone(), TABLE, FIELD, 2);
    let mut late = SQLiteVectorIndex::new(peer.clone(), TABLE, FIELD, 2);
    for creator_first in [false, true] {
        raw.clear().unwrap();
        raw.add(1, vec![1.0, 0.0]).unwrap();
        peer.begin_transaction().unwrap();
        late.add(99, vec![0.0, 1.0]).unwrap();
        connection.begin_transaction().unwrap();
        canonical(&connection, TABLE, FIELD, 2)
            .create_index(
                &definition.relation,
                &Resolver,
                options,
                &temporary,
                &control,
            )
            .unwrap();
        if creator_first {
            connection.commit_transaction().unwrap();
            assert!(peer.commit_transaction().is_err());
            peer.rollback_transaction().unwrap();
            scores(&runtime(&connection, &temporary, &control), &[(1, 1.0)]);
        } else {
            peer.commit_transaction().unwrap();
            assert!(connection.commit_transaction().is_err());
            connection.rollback_transaction().unwrap();
            assert_eq!(raw.count().unwrap(), 2);
            assert!(capture(&connection, &control)
                .selected_source(&Resolver, &control)
                .unwrap()
                .is_none());
        }
    }
}

#[test]
fn native_diskann_runtime_lifecycle_preserves_transactions_and_cold_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.db");
        let (generation, held, rebuilt, empty) = {
            let connection = open(&path, mode);
            let control = StorageReadControl::with_limit(1 << 21);
            let temporary = DiskANNTemporaryBudget::new(1 << 20);
            let options = diskann_runtime_fixture_options(2).unwrap();
            create(&connection, &temporary, &control);
            let mut index = runtime(&connection, &temporary, &control);
            scores(&index, &[(1, 1.0), (2, 1.0), (3, -1.0)]);
            assert_eq!(index.count().unwrap(), 4);
            let held = index.snapshot().unwrap();
            let first = capture(&connection, &control)
                .selected_source(&Resolver, &control)
                .unwrap()
                .unwrap()
                .generation();
            connection.savepoint("runtime-user").unwrap();
            index.add(1, vec![-1.0, 0.0]).unwrap();
            index.delete(2).unwrap();
            index.add(4, vec![0.0, 1.0]).unwrap();
            index.initialize().unwrap();
            let rebuilt = index.snapshot().unwrap();
            scores(&*rebuilt, &[(1, -1.0), (3, -1.0), (4, 0.0)]);
            index.clear().unwrap();
            let empty = index.snapshot().unwrap();
            assert_eq!(empty.count().unwrap(), 0);
            let selected = capture(&connection, &control)
                .into_vector_index(&Resolver, options.read, &control)
                .unwrap()
                .unwrap();
            assert_eq!(selected.manifest().input().nodes, 0);
            connection.rollback_to_savepoint("runtime-user").unwrap();
            scores(&index, &[(1, 1.0), (2, 1.0), (3, -1.0)]);
            assert_eq!(
                capture(&connection, &control)
                    .selected_source(&Resolver, &control)
                    .unwrap()
                    .unwrap()
                    .generation(),
                first
            );
            connection.commit_transaction().unwrap();
            assert!(index.clear().is_err());
            assert!(index.initialize().is_err());
            connection.begin_transaction().unwrap();
            let mut bad = options;
            bad.generation.max_record_bytes = 1;
            let constrained_handle = canonical(&connection, TABLE, FIELD, 2)
                .bind(row().relation, Arc::new(Resolver), bad.read, &control)
                .unwrap();
            let mut constrained =
                PersistentDiskANNIndex::new(constrained_handle, bad, &temporary).unwrap();
            assert!(constrained.clear().is_err());
            scores(&index, &[(1, 1.0), (2, 1.0), (3, -1.0)]);
            index.clear().unwrap();
            index.add(5, vec![1.0, 0.0]).unwrap();
            index.initialize().unwrap();
            connection.commit_transaction().unwrap();
            scores(&index, &[(5, 1.0)]);
            assert_eq!(temporary.used(), 0);
            let final_generation = capture(&connection, &control)
                .selected_source(&Resolver, &control)
                .unwrap()
                .unwrap()
                .generation();
            (final_generation, held, rebuilt, empty)
        };
        // Old private and committed readers outlive their writable native connection.
        scores(&*held, &[(1, 1.0), (2, 1.0), (3, -1.0)]);
        scores(&*rebuilt, &[(1, -1.0), (3, -1.0), (4, 0.0)]);
        assert_eq!(empty.count().unwrap(), 0);
        drop((held, rebuilt, empty));
        let connection = open(&path, mode);
        let control = StorageReadControl::with_limit(1 << 20);
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let mut index = runtime(&connection, &temporary, &control);
        assert_eq!(
            capture(&connection, &control)
                .selected_source(&Resolver, &control)
                .unwrap()
                .unwrap()
                .generation(),
            generation
        );
        scores(&index, &[(5, 1.0)]);
        index.add(6, vec![0.0, 1.0]).unwrap();
        scores(&index, &[(5, 1.0), (6, 0.0)]);
    }
}
