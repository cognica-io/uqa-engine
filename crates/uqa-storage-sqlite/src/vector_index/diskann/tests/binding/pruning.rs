//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::change_count;
use super::{
    canonical, capture, identity::Resolver, open, publication::build, row, setup,
    ManagedConnection, StorageReadControl, FIELD, TABLE,
};
use uqa_storage::diskann_index::{
    changes::{DiskANNPruneCursor, DiskANNPruneRequest, DiskANNPruneResult},
    format::DiskANNGeneration,
};
use uqa_storage::key_value::KeyValueDiskANNPruner;

fn page(max_records: usize) -> DiskANNPruneRequest {
    DiskANNPruneRequest {
        after: None,
        max_records,
    }
}

fn prune(
    connection: &ManagedConnection,
    pruner: &KeyValueDiskANNPruner,
    request: DiskANNPruneRequest,
    control: &StorageReadControl,
) -> uqa_storage::StorageBackendResult<DiskANNPruneResult> {
    connection.prune_diskann_changes(
        &capture(connection, control),
        &Resolver,
        pruner,
        request,
        control,
    )
}

#[test]
fn native_diskann_pruning_preserves_late_writes_retained_views_and_cold_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pruning.db");
        let generation = {
            let connection = open(&path, mode);
            let peer = open(&path, mode);
            let catalog = setup(&connection);
            let mut row = row();
            row.definition_json = Some(serde_json::to_string(&[82; 16]).unwrap());
            catalog.save_catalog_index_row(&row).unwrap();
            let control = StorageReadControl::with_limit(1 << 20);
            let canonical = canonical(&connection, TABLE, FIELD, 2);
            let original = canonical.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
            canonical.replace(2, &[], &control).unwrap();
            canonical.replace(3, &[], &control).unwrap();
            canonical.replace(3, &[vec![0.0, 1.0]], &control).unwrap();
            peer.begin_transaction().unwrap();
            let late = super::canonical(&peer, TABLE, FIELD, 2)
                .replace(4, &[], &control)
                .unwrap();
            let (coverage, stage) = build(&connection, &control);
            let repository = connection.diskann_generations(&control).unwrap();
            let pruner = KeyValueDiskANNPruner::open(
                repository
                    .open_source(stage.generation(), &control)
                    .unwrap(),
                8192,
                &control,
            )
            .unwrap();
            connection.begin_transaction().unwrap();
            connection
                .publish_diskann_generation(&coverage, &Resolver, &control)
                .unwrap();
            assert!(prune(&connection, &pruner, page(64), &control).is_err());
            connection.rollback_transaction().unwrap();
            connection
                .publish_diskann_generation(&coverage, &Resolver, &control)
                .unwrap();
            let changed = canonical.replace(1, &[vec![0.0, 1.0]], &control).unwrap();
            peer.commit_transaction().unwrap();
            let old_cursor = prune_pass(&connection, &canonical, &pruner, &control);
            assert_eq!(
                coverage.source().origin(1, &control).unwrap(),
                Some(original)
            );
            assert_eq!(
                coverage
                    .source()
                    .next_change_after(None, &control)
                    .unwrap()
                    .unwrap()
                    .document(),
                1
            );
            let current = capture(&connection, &control);
            assert_eq!(
                current
                    .next_change_after(None, &control)
                    .unwrap()
                    .unwrap()
                    .version(),
                changed
            );
            assert_eq!(
                current
                    .next_change_after(Some(1), &control)
                    .unwrap()
                    .unwrap()
                    .version(),
                late
            );
            publication_races(&connection, &peer, &pruner, old_cursor, &control)
        };
        verify_reopen(&open(&path, mode), generation);
    }
}

fn prune_pass(
    connection: &ManagedConnection,
    canonical: &crate::vector_index::SQLiteDiskANNCanonical,
    pruner: &KeyValueDiskANNPruner,
    control: &StorageReadControl,
) -> Option<DiskANNPruneCursor> {
    assert_eq!(change_count(connection), 6);
    connection.begin_transaction().unwrap();
    connection.savepoint("pruning").unwrap();
    assert_eq!(
        prune(connection, pruner, page(64), control)
            .unwrap()
            .removed,
        4
    );
    connection.rollback_to_savepoint("pruning").unwrap();
    canonical.replace(1, &[], control).unwrap();
    assert!(prune(connection, pruner, page(64), control).is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(change_count(connection), 6);
    let mut request = page(1);
    let mut totals = (0, 0);
    let old_cursor;
    loop {
        let result = prune(connection, pruner, request, control).unwrap();
        assert!(result.examined <= 1 && result.removed <= 1);
        totals.0 += result.examined;
        totals.1 += result.removed;
        if result.next.is_none() {
            old_cursor = request.after;
            break;
        }
        request.after = result.next;
    }
    assert_eq!(totals, (6, 4));
    old_cursor
}

fn publication_races(
    connection: &ManagedConnection,
    peer: &ManagedConnection,
    pruner: &KeyValueDiskANNPruner,
    old_cursor: Option<DiskANNPruneCursor>,
    control: &StorageReadControl,
) -> DiskANNGeneration {
    let canonical = canonical(connection, TABLE, FIELD, 2);
    let repository = connection.diskann_generations(control).unwrap();
    let mut definition = row();
    definition.definition_json = Some(serde_json::to_string(&[82; 16]).unwrap());
    canonical.replace(0, &[], control).unwrap();
    canonical.replace(0, &[], control).unwrap();
    assert_eq!(
        prune(
            connection,
            pruner,
            DiskANNPruneRequest {
                after: old_cursor,
                max_records: 64
            },
            control
        )
        .unwrap()
        .examined,
        0
    );
    let (new_coverage, new_stage) = build(connection, control);
    connection.begin_transaction().unwrap();
    assert_eq!(
        prune(connection, pruner, page(64), control)
            .unwrap()
            .removed,
        1
    );
    peer.publish_diskann_generation(&new_coverage, &Resolver, control)
        .unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(change_count(connection), 4);
    assert!(prune(connection, pruner, page(64), control).is_err());
    let pruner = KeyValueDiskANNPruner::open(
        repository
            .open_source(new_stage.generation(), control)
            .unwrap(),
        8192,
        control,
    )
    .unwrap();
    assert!(prune(
        connection,
        &pruner,
        DiskANNPruneRequest {
            after: old_cursor,
            max_records: 64
        },
        control
    )
    .is_err());
    connection.begin_transaction().unwrap();
    prune(connection, &pruner, page(64), control).unwrap();
    crate::Catalog::open(peer.clone())
        .unwrap()
        .save_catalog_index_row(&definition)
        .unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(change_count(connection), 4);
    assert!(prune(connection, &pruner, page(0), control).is_err());
    let tiny = StorageReadControl::with_limit(1);
    assert!(connection
        .prune_diskann_changes(
            &capture(connection, control),
            &Resolver,
            &pruner,
            page(64),
            &tiny
        )
        .is_err());
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    assert!(connection
        .prune_diskann_changes(
            &capture(connection, control),
            &Resolver,
            &pruner,
            page(64),
            &cancelled
        )
        .is_err());
    canonical.replace(9, &[], control).unwrap();
    assert_eq!(
        prune(connection, &pruner, page(64), control)
            .unwrap()
            .removed,
        4
    );
    assert_eq!(change_count(connection), 1);
    new_stage.generation()
}

fn verify_reopen(connection: &ManagedConnection, generation: DiskANNGeneration) {
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = connection.diskann_generations(&control).unwrap();
    let pruner = KeyValueDiskANNPruner::open(
        repository.open_source(generation, &control).unwrap(),
        8192,
        &control,
    )
    .unwrap();
    let result = prune(connection, &pruner, page(64), &control).unwrap();
    assert_eq!((result.examined, result.removed, result.next), (1, 0, None));
    assert_eq!(
        capture(connection, &control)
            .next_change_after(None, &control)
            .unwrap()
            .unwrap()
            .document(),
        9
    );
    assert_eq!(change_count(connection), 1);
}
