//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    canonical, capture, identity::Resolver, open, publication::build, row, setup, FIELD, TABLE,
};
use crate::{Catalog, ManagedConnection, SQLiteDiskANNHandle};
use std::sync::Arc;
use uqa_storage::{
    diskann_index::{format::PAGE_BYTES, pages::DiskANNReadLimits},
    read_control::StorageReadControl,
    VectorIndex,
};

fn live(
    connection: &ManagedConnection,
    control: &StorageReadControl,
) -> uqa_storage::StorageBackendResult<SQLiteDiskANNHandle> {
    canonical(connection, TABLE, FIELD, 2).bind(
        row().relation,
        Arc::new(Resolver),
        DiskANNReadLimits {
            resident_bytes: 65_536,
            cache_bytes: PAGE_BYTES,
            max_in_flight_page_bytes: 2 * PAGE_BYTES,
            max_record_bytes: 8192,
        },
        control,
    )
}

fn definition() -> uqa_storage::CatalogIndexRow {
    let mut row = row();
    row.definition_json = Some(serde_json::to_string(&[82; 16]).unwrap());
    row
}

fn scores(index: &dyn VectorIndex, expected: &[(u64, f32)]) {
    let actual = index.search_knn(&[1.0, 0.0], 10).unwrap();
    assert_eq!(
        actual
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|&(document, score)| (document, f64::from(score).to_bits()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn native_diskann_live_writes_keep_actual_catalog_visibility_and_cold_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live.db");
        let generation = {
            let connection = open(&path, mode);
            let catalog = setup(&connection);
            catalog.save_catalog_index_row(&definition()).unwrap();
            let control = StorageReadControl::with_limit(1 << 21);
            let canonical = canonical(&connection, TABLE, FIELD, 2);
            canonical.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
            canonical.replace(2, &[], &control).unwrap();
            assert!(live(&connection, &control).is_err());
            let (coverage, stage) = build(&connection, &control);
            connection
                .publish_diskann_generation(&coverage, &Resolver, &control)
                .unwrap();
            drop((coverage, stage));
            let index = live(&connection, &control).unwrap();
            let held = index.snapshot().unwrap();
            scores(&held, &[(1, 1.0)]);

            connection.begin_transaction().unwrap();
            connection.savepoint("live-write").unwrap();
            let undone = index.replace(1, &[vec![-1.0, 0.0]]).unwrap();
            index.replace(2, &[vec![0.0, 1.0], vec![1.0, 0.0]]).unwrap();
            let private = index.snapshot().unwrap();
            scores(&private, &[(1, -1.0), (2, 1.0)]);
            assert_eq!(private.count().unwrap(), 3);
            assert!(index.replace(1, &[vec![1.0]]).is_err());
            assert_eq!(
                capture(&connection, &control).origin(1, &control).unwrap(),
                Some(undone)
            );
            connection.rollback_to_savepoint("live-write").unwrap();
            scores(&index.snapshot().unwrap(), &[(1, 1.0)]);
            scores(&private, &[(1, -1.0), (2, 1.0)]);
            assert_ne!(undone, index.replace(1, &[vec![0.0, 1.0]]).unwrap());
            connection.commit_transaction().unwrap();
            scores(&index.snapshot().unwrap(), &[(1, 0.0)]);
            scores(&held, &[(1, 1.0)]);

            let peer = open(&path, mode);
            concurrent_writers(&connection, &peer, &index, &control);

            connection.begin_transaction().unwrap();
            catalog.save_catalog_index_row(&definition()).unwrap();
            let (coverage, stage) = build(&connection, &control);
            connection
                .publish_diskann_generation(&coverage, &Resolver, &control)
                .unwrap();
            let promoted = live(&connection, &control).unwrap();
            connection.commit_transaction().unwrap();
            promoted.replace(5, &[vec![0.0, 1.0]]).unwrap();
            let generation = stage.generation();
            assert_eq!(
                promoted.snapshot().unwrap().manifest().input().generation,
                generation
            );
            scores(&held, &[(1, 1.0)]);
            scores(&private, &[(1, -1.0), (2, 1.0)]);

            let reserved = control
                .memory()
                .reserve(control.memory().limit() - control.memory().used())
                .unwrap();
            assert!(index.replace(9, &[vec![1.0, 0.0]]).is_err());
            assert!(index.snapshot().is_err());
            drop(reserved);
            assert!(capture(&connection, &control)
                .origin(9, &control)
                .unwrap()
                .is_none());
            guards(&connection, &peer, &index, &control);
            let tiny = StorageReadControl::with_limit(1);
            assert!(live(&connection, &tiny).is_err());
            assert_eq!(tiny.memory().used(), 0);
            scores(
                &index.snapshot().unwrap(),
                &[(1, 1.0), (3, 1.0), (4, -1.0), (5, 0.0)],
            );
            control.cancellation().cancel();
            assert!(index.replace(1, &[]).is_err());
            assert!(index.snapshot().is_err());
            generation
        };
        let connection = open(&path, mode);
        let control = StorageReadControl::with_limit(1 << 20);
        let index = live(&connection, &control).unwrap();
        let before = index.snapshot().unwrap();
        assert_eq!(before.manifest().input().generation, generation);
        scores(&before, &[(1, 1.0), (3, 1.0), (4, -1.0), (5, 0.0)]);
        index.replace(1, &[]).unwrap();
        index.replace(6, &[vec![1.0, 0.0]]).unwrap();
        let after = index.snapshot().unwrap();
        scores(&after, &[(3, 1.0), (4, -1.0), (5, 0.0), (6, 1.0)]);
        assert_eq!(after.count().unwrap(), 4);
        assert!(!after.contains_document(1).unwrap());
        drop((index, connection));
        scores(&before, &[(1, 1.0), (3, 1.0), (4, -1.0), (5, 0.0)]);
        scores(&after, &[(3, 1.0), (4, -1.0), (5, 0.0), (6, 1.0)]);
    }
}

fn concurrent_writers(
    connection: &ManagedConnection,
    peer: &ManagedConnection,
    index: &SQLiteDiskANNHandle,
    control: &StorageReadControl,
) {
    let other = live(peer, control).unwrap();
    connection.begin_transaction().unwrap();
    peer.begin_transaction().unwrap();
    index.replace(3, &[vec![1.0, 0.0]]).unwrap();
    other.replace(4, &[vec![-1.0, 0.0]]).unwrap();
    connection.commit_transaction().unwrap();
    peer.commit_transaction().unwrap();
    scores(&index.snapshot().unwrap(), &[(1, 0.0), (3, 1.0), (4, -1.0)]);
    connection.begin_transaction().unwrap();
    peer.begin_transaction().unwrap();
    index.replace(1, &[vec![1.0, 0.0]]).unwrap();
    other.replace(1, &[vec![-1.0, 0.0]]).unwrap();
    connection.commit_transaction().unwrap();
    assert!(peer.commit_transaction().is_err());
    peer.rollback_transaction().unwrap();
    scores(&other.snapshot().unwrap(), &[(1, 1.0), (3, 1.0), (4, -1.0)]);
}

fn guards(
    connection: &ManagedConnection,
    peer: &ManagedConnection,
    index: &SQLiteDiskANNHandle,
    control: &StorageReadControl,
) {
    connection.begin_transaction().unwrap();
    index.replace(9, &[vec![1.0, 0.0]]).unwrap();
    Catalog::open(peer.clone())
        .unwrap()
        .save_catalog_index_row(&definition())
        .unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert!(!index.snapshot().unwrap().contains_document(9).unwrap());
    let catalog = Catalog::open(connection.clone()).unwrap();
    for kind in 0..3 {
        connection.begin_transaction().unwrap();
        match kind {
            0 => {
                let mut changed = definition();
                catalog.drop_catalog_index(&changed.relation).unwrap();
                changed.definition_json = Some(serde_json::to_string(&[83; 16]).unwrap());
                catalog.save_catalog_index_row(&changed).unwrap();
            }
            1 => {
                let mut changed = definition();
                let mut params =
                    uqa_storage::vector_index::DiskANNIndexParams::for_dimensions(2).unwrap();
                params.search_list_size += 1;
                changed.parameters_json =
                    serde_json::to_string(&params.to_catalog_map(2).unwrap()).unwrap();
                catalog.save_catalog_index_row(&changed).unwrap();
            }
            _ => {
                let mut table = catalog
                    .load_tables()
                    .unwrap()
                    .into_iter()
                    .find(|table| table.relation.qualified_name() == TABLE)
                    .unwrap();
                table.storage_generation = [73; 16];
                catalog.save_table(&table).unwrap();
            }
        }
        assert!(index.replace(9, &[vec![1.0, 0.0]]).is_err());
        assert!(index.snapshot().is_err());
        connection.rollback_transaction().unwrap();
        assert!(capture(connection, control)
            .origin(9, control)
            .unwrap()
            .is_none());
    }
}
