//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    canonical, capture, identity::Resolver, open, row, setup, ManagedConnection,
    RetainedSQLiteDiskANNCanonical, StorageReadControl, FIELD, TABLE,
};
use uqa_storage::diskann_index::{
    build::DiskANNCanonicalCoverage, format::DiskANNGeneration, pages::DiskANNOriginReader,
};
use uqa_storage::key_value::{
    conformance::build_diskann_publication_fixture, publication::selected_generation,
    DiskANNStageStatus, KeyValueDiskANNStage,
};

pub(super) fn build(
    connection: &ManagedConnection,
    control: &StorageReadControl,
) -> (
    DiskANNCanonicalCoverage<RetainedSQLiteDiskANNCanonical>,
    KeyValueDiskANNStage,
) {
    let source = capture(connection, control);
    let scope = source.index_scope(&Resolver, control).unwrap();
    let parameters = source.index_parameters().unwrap();
    let repository = connection.diskann_generations(control).unwrap();
    repository.initialize(control).unwrap();
    let mut stage = repository.allocate_bound_stage(&scope, control).unwrap();
    let coverage =
        build_diskann_publication_fixture(source, &mut stage, parameters, control).unwrap();
    (coverage, stage)
}

fn selected(
    connection: &ManagedConnection,
    control: &StorageReadControl,
) -> Option<DiskANNGeneration> {
    let source = capture(connection, control);
    let scope = source.index_scope(&Resolver, control).unwrap();
    let snapshot = connection.native_snapshot().unwrap().unwrap();
    let read = snapshot.record_read();
    let physical = crate::diskann::map_read(&read, snapshot.database).unwrap();
    selected_generation(&scope, &physical, control).unwrap()
}

#[test]
fn native_diskann_publication_preserves_atomic_visibility_and_cold_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("publication.db");
        let generation = {
            let connection = open(&path, mode);
            let catalog = setup(&connection);
            let mut row = row();
            row.definition_json = Some(serde_json::to_string(&[82; 16]).unwrap());
            catalog.save_catalog_index_row(&row).unwrap();
            let control = StorageReadControl::with_limit(1 << 20);
            let canonical = canonical(&connection, TABLE, FIELD, 2);
            let original = canonical.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
            canonical.replace(2, &[], &control).unwrap();
            let (first, first_stage) = build(&connection, &control);
            let (stale, _) = build(&connection, &control);
            let peer = open(&path, mode);
            connection.begin_transaction().unwrap();
            connection
                .publish_diskann_generation(&first, &Resolver, &control)
                .unwrap();
            assert_eq!(
                selected(&connection, &control),
                Some(first_stage.generation())
            );
            assert_eq!(selected(&peer, &control), None);
            connection.rollback_transaction().unwrap();
            assert_eq!(selected(&connection, &control), None);
            assert_eq!(
                first_stage.status(&control).unwrap(),
                Some(DiskANNStageStatus::Sealed)
            );
            connection
                .publish_diskann_generation(&first, &Resolver, &control)
                .unwrap();
            assert!(connection
                .publish_diskann_generation(&stale, &Resolver, &control)
                .is_err());
            let source = connection
                .diskann_generations(&control)
                .unwrap()
                .open_source(first_stage.generation(), &control)
                .unwrap();
            let reader = DiskANNOriginReader::open(source, 8192, &control).unwrap();
            assert_eq!(
                reader.origin(1, &control).unwrap().unwrap().version(),
                original
            );
            private_and_conflict(&connection, &peer, &control)
        };
        let connection = open(&path, mode);
        let control = StorageReadControl::with_limit(1 << 20);
        assert_eq!(selected(&connection, &control), Some(generation));
        let canonical = capture(&connection, &control);
        let source = connection
            .diskann_generations(&control)
            .unwrap()
            .open_source(generation, &control)
            .unwrap();
        let reader = DiskANNOriginReader::open(source, 8192, &control).unwrap();
        for document in [1, 2] {
            assert_eq!(
                reader
                    .origin(document, &control)
                    .unwrap()
                    .map(uqa_storage::diskann_index::format::DiskANNCanonicalOrigin::version),
                canonical.origin(document, &control).unwrap()
            );
        }
    }
}

fn private_and_conflict(
    connection: &ManagedConnection,
    peer: &ManagedConnection,
    control: &StorageReadControl,
) -> DiskANNGeneration {
    let canonical = canonical(connection, TABLE, FIELD, 2);
    connection.begin_transaction().unwrap();
    connection.savepoint("publication").unwrap();
    canonical.replace(2, &[vec![0.0, 0.0]], control).unwrap();
    let (undone, _) = build(connection, control);
    connection.rollback_to_savepoint("publication").unwrap();
    assert!(connection
        .publish_diskann_generation(&undone, &Resolver, control)
        .is_err());
    connection.rollback_transaction().unwrap();
    connection.begin_transaction().unwrap();
    canonical.replace(2, &[vec![0.0, 0.0]], control).unwrap();
    let (private, private_stage) = build(connection, control);
    connection
        .publish_diskann_generation(&private, &Resolver, control)
        .unwrap();
    connection.commit_transaction().unwrap();
    let (winner, winner_stage) = build(connection, control);
    let (loser, loser_stage) = build(peer, control);
    connection.begin_transaction().unwrap();
    peer.begin_transaction().unwrap();
    connection
        .publish_diskann_generation(&winner, &Resolver, control)
        .unwrap();
    peer.publish_diskann_generation(&loser, &Resolver, control)
        .unwrap();
    connection.commit_transaction().unwrap();
    assert!(peer.commit_transaction().is_err());
    peer.rollback_transaction().unwrap();
    assert_eq!(
        private_stage.status(control).unwrap(),
        Some(DiskANNStageStatus::Retired)
    );
    assert_eq!(
        loser_stage.status(control).unwrap(),
        Some(DiskANNStageStatus::Sealed)
    );
    assert_eq!(
        selected(connection, control),
        Some(winner_stage.generation())
    );
    winner_stage.generation()
}
