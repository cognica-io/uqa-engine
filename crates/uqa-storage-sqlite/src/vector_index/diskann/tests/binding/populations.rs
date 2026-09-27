//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual native sessions preserve generation populations across writes, refresh and publication.

use super::{
    canonical, capture, identity::Resolver, open, publication::build, row, setup, FIELD, TABLE,
};
use crate::{ManagedConnection, SQLiteDiskANNHandle};
use std::sync::Arc;
use uqa_storage::{
    diskann_index::{
        format::DiskANNGeneration, DiskANNCanonicalCounts, DiskANNCanonicalRead,
        DiskANNPersistentOwner,
    },
    key_value::conformance::diskann_runtime_fixture_options,
    read_control::StorageReadControl,
    VectorIndex,
};

mod lifetimes;
mod rename;

fn live(connection: &ManagedConnection, control: &StorageReadControl) -> SQLiteDiskANNHandle {
    canonical(connection, TABLE, FIELD, 2)
        .bind(
            row().relation,
            Arc::new(Resolver),
            diskann_runtime_fixture_options(2).unwrap().read,
            control,
        )
        .unwrap()
}
fn counts(live: &SQLiteDiskANNHandle, current: u64, changed: u64) {
    assert_eq!(
        live.query_metadata().unwrap().canonical_counts,
        Some(DiskANNCanonicalCounts::new(current, changed).unwrap())
    );
}
fn seed(connection: &ManagedConnection, control: &StorageReadControl) -> DiskANNGeneration {
    let catalog = setup(connection);
    let mut definition = row();
    definition.definition_json = Some(serde_json::to_string(&[82; 16]).unwrap());
    catalog.save_catalog_index_row(&definition).unwrap();
    let canonical = canonical(connection, TABLE, FIELD, 2);
    canonical.replace(1, &[vec![1.0, 0.0]], control).unwrap();
    canonical
        .replace(2, &[vec![0.0, 1.0], vec![1.0, 0.0]], control)
        .unwrap();
    canonical.replace(3, &[], control).unwrap();
    rebuild(connection, control)
}
fn rebuild(connection: &ManagedConnection, control: &StorageReadControl) -> DiskANNGeneration {
    let (coverage, stage) = build(connection, control);
    connection
        .publish_diskann_generation(&coverage, &Resolver, control)
        .unwrap();
    stage.generation()
}
fn retained_counts(
    index: &dyn VectorIndex,
    current: u64,
    changed: u64,
    control: &StorageReadControl,
) {
    assert_eq!(
        index
            .diskann_query_metadata(control)
            .unwrap()
            .unwrap()
            .canonical_counts,
        Some(DiskANNCanonicalCounts::new(current, changed).unwrap())
    );
}

#[test]
fn native_diskann_population_writers_merge_both_orders_and_refresh_with_retained_views() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("populations.db");
        let connection = open(&path, mode);
        let control = StorageReadControl::with_limit(1 << 22);
        seed(&connection, &control);
        let peer = open(&path, mode);
        let index = live(&connection, &control);
        let other = live(&peer, &control);
        for reverse in [false, true] {
            for refresh in [false, true] {
                index.replace(1, &[vec![1.0, 0.0]]).unwrap();
                index.replace(2, &[vec![0.0, 1.0], vec![1.0, 0.0]]).unwrap();
                rebuild(&connection, &control);
                counts(&index, 3, 0);
                connection.begin_transaction().unwrap();
                peer.begin_transaction().unwrap();
                index
                    .replace(1, &[vec![1.0, 0.0], vec![-1.0, 0.0]])
                    .unwrap();
                other.replace(2, &[vec![0.0, 1.0]]).unwrap();
                counts(&index, 4, 2);
                counts(&other, 2, 1);
                let held = index.snapshot().unwrap();
                let (first, second, remaining) = if reverse {
                    (&peer, &connection, &index)
                } else {
                    (&connection, &peer, &other)
                };
                first.commit_transaction().unwrap();
                if refresh {
                    second
                        .refresh_transaction_snapshot(control.cancellation())
                        .unwrap();
                    counts(remaining, 3, 3);
                }
                second.commit_transaction().unwrap();
                counts(&index, 3, 3);
                retained_counts(&held, 4, 2, &control);
            }
        }
        connection.begin_transaction().unwrap();
        peer.begin_transaction().unwrap();
        index.replace(1, &[vec![1.0, 0.0]]).unwrap();
        other.replace(1, &[]).unwrap();
        connection.commit_transaction().unwrap();
        assert!(peer.commit_transaction().is_err());
        peer.rollback_transaction().unwrap();
        counts(&other, 2, 2);
    }
}

#[test]
fn native_diskann_population_publication_censuses_late_origins_in_both_commit_orders() {
    for mode in 0..4 {
        for writer_first in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("late-populations.db");
            let connection = open(&path, mode);
            let control = StorageReadControl::with_limit(1 << 22);
            seed(&connection, &control);
            let index = live(&connection, &control);
            let original = index.snapshot().unwrap();
            let peer = open(&path, mode);
            let writer = live(&peer, &control);
            let (coverage, stage) = build(&connection, &control);
            connection.begin_transaction().unwrap();
            peer.begin_transaction().unwrap();
            connection
                .publish_diskann_generation(&coverage, &Resolver, &control)
                .unwrap();
            writer.replace(2, &[vec![1.0, 0.0]]).unwrap();
            let held = index.snapshot().unwrap();
            let (first, second) = if writer_first {
                (&peer, &connection)
            } else {
                (&connection, &peer)
            };
            first.commit_transaction().unwrap();
            second.commit_transaction().unwrap();
            counts(&index, 2, 1);
            assert_eq!(
                capture(&connection, &control)
                    .population_counts(stage.generation(), &control)
                    .unwrap(),
                Some(DiskANNCanonicalCounts::new(2, 1).unwrap())
            );
            retained_counts(&held, 3, 0, &control);
            retained_counts(&original, 3, 0, &control);
        }
    }
}

#[test]
fn native_diskann_population_binding_initialization_keeps_undo_empty_origins_and_cold_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("population-initialization.db");
        let generation = {
            let connection = open(&path, mode);
            let control = StorageReadControl::with_limit(1 << 22);
            let generation = seed(&connection, &control);
            // A predecessor generation lacks only this derived metadata; its canonical origins and graph stay intact.
            connection
                .with_native_write(|snapshot, batch| {
                    let owner = snapshot.table_owner(TABLE)?.unwrap();
                    for family in [
                        crate::mvcc::native::NativeRecordFamily::VectorPopulations,
                        crate::mvcc::native::NativeRecordFamily::VectorPopulationWitnesses,
                    ] {
                        snapshot.delete_prefix(
                            batch,
                            family,
                            owner,
                            &[rusqlite::types::ValueRef::Text(FIELD.as_bytes())],
                        )?;
                    }
                    Ok(())
                })
                .unwrap()
                .unwrap();
            let stale = capture(&connection, &control);
            assert_eq!(stale.population_counts(generation, &control).unwrap(), None);
            connection.begin_transaction().unwrap();
            counts(&live(&connection, &control), 3, 0);
            connection.rollback_transaction().unwrap();
            assert_eq!(
                capture(&connection, &control)
                    .population_counts(generation, &control)
                    .unwrap(),
                None
            );
            connection.begin_transaction().unwrap();
            let index = live(&connection, &control);
            index.replace(1, &[vec![1.0, 0.0]]).unwrap();
            index.replace(3, &[]).unwrap();
            counts(&index, 3, 1);
            connection.commit_transaction().unwrap();
            assert_eq!(stale.population_counts(generation, &control).unwrap(), None);
            generation
        };
        let connection = open(&path, mode);
        let control = StorageReadControl::with_limit(1 << 22);
        let index = live(&connection, &control);
        counts(&index, 3, 1);
        assert_eq!(
            capture(&connection, &control)
                .population_counts(generation, &control)
                .unwrap(),
            Some(DiskANNCanonicalCounts::new(3, 1).unwrap())
        );
        index.replace(2, &[]).unwrap();
        counts(&index, 1, 1);
    }
}
