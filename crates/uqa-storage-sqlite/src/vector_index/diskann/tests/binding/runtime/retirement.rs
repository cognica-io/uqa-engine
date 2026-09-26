//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{diskann_index::format::DiskANNGeneration, key_value::DiskANNStageStatus};

fn selected(connection: &ManagedConnection, control: &StorageReadControl) -> DiskANNGeneration {
    capture(connection, control)
        .selected_source(&Resolver, control)
        .unwrap()
        .unwrap()
        .generation()
}

fn retire_and_recreate(
    connection: &ManagedConnection,
    private: bool,
) -> ((DiskANNGeneration, DiskANNGeneration), Arc<dyn VectorIndex>) {
    let catalog = setup(connection);
    let mut definition = row();
    definition.definition_json = Some(serde_json::to_string(&[82; 16]).unwrap());
    catalog.save_catalog_index_row(&definition).unwrap();
    let control = StorageReadControl::with_limit(1 << 21);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let options = diskann_runtime_fixture_options(2).unwrap();
    let canonical = canonical(connection, TABLE, FIELD, 2);
    let mut raw = SQLiteVectorIndex::new(connection.clone(), TABLE, FIELD, 2);
    raw.add(1, vec![1.0, 0.0]).unwrap();
    raw.add_many(2, vec![vec![-1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    assert!(canonical
        .retire_index(&definition.relation, &Resolver, &control)
        .is_err());
    connection.begin_transaction().unwrap();
    canonical
        .create_index(
            &definition.relation,
            &Resolver,
            options,
            &temporary,
            &control,
        )
        .unwrap();
    if !private {
        connection.commit_transaction().unwrap();
        connection.begin_transaction().unwrap();
    }
    let mut stale = runtime(connection, &temporary, &control);
    if private {
        let obsolete = capture(connection, &control);
        stale.initialize().unwrap();
        assert!(connection
            .with_native_write(|snapshot, batch| {
                obsolete.retire_generation(&Resolver, snapshot, batch, &control)?;
                Ok(())
            })
            .is_err());
    }
    let held = stale.snapshot().unwrap();
    let first = selected(connection, &control);
    catalog
        .set_metadata("retirement-outer-write", "kept")
        .unwrap();
    connection.savepoint("before-retirement").unwrap();
    canonical
        .retire_index(&definition.relation, &Resolver, &control)
        .unwrap();
    assert!(connection
        .diskann_generations(&control)
        .unwrap()
        .reclaim_retired_step(first, 1, &control)
        .is_err());
    assert!(stale.snapshot().is_err());
    assert!(stale.add(9, vec![1.0, 0.0]).is_err());
    assert_eq!(raw.count().unwrap(), 3);
    catalog.drop_catalog_index(&definition.relation).unwrap();
    scores(&*held, &[(1, 1.0), (2, 0.0)]);
    connection
        .rollback_to_savepoint("before-retirement")
        .unwrap();
    assert_eq!(selected(connection, &control), first);
    scores(&stale, &[(1, 1.0), (2, 0.0)]);
    canonical
        .retire_index(&definition.relation, &Resolver, &control)
        .unwrap();
    catalog.drop_catalog_index(&definition.relation).unwrap();
    definition.definition_json = Some(serde_json::to_string(&[83; 16]).unwrap());
    catalog.save_catalog_index_row(&definition).unwrap();
    canonical
        .create_index(
            &definition.relation,
            &Resolver,
            options,
            &temporary,
            &control,
        )
        .unwrap();
    let replacement = selected(connection, &control);
    assert_ne!(replacement.index(), first.index());
    assert!(stale.add(9, vec![1.0, 0.0]).is_err());
    connection.commit_transaction().unwrap();
    assert_eq!(
        catalog
            .get_metadata("retirement-outer-write")
            .unwrap()
            .as_deref(),
        Some("kept")
    );
    scores(&*held, &[(1, 1.0), (2, 0.0)]);
    ((first, replacement), held)
}

#[test]
fn native_diskann_runtime_retirement_preserves_undo_recreation_and_cold_reopen() {
    for (mode, private) in (0..4).flat_map(|mode| [false, true].map(|private| (mode, private))) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("retirement.db");
        let ((first, replacement), held) = {
            let connection = open(&path, mode);
            retire_and_recreate(&connection, private)
        };
        scores(&*held, &[(1, 1.0), (2, 0.0)]);
        drop(held);
        let connection = open(&path, mode);
        let control = StorageReadControl::with_limit(1 << 21);
        let temporary = DiskANNTemporaryBudget::new(1 << 20);
        let repository = connection.diskann_generations(&control).unwrap();
        assert_eq!(
            repository
                .resume_stage(first, &control)
                .unwrap()
                .status(&control)
                .unwrap(),
            Some(DiskANNStageStatus::Retired)
        );
        assert_eq!(selected(&connection, &control), replacement);
        connection.vacuum().unwrap();
        assert!(repository.resume_stage(first, &control).is_err());
        scores(
            &runtime(&connection, &temporary, &control),
            &[(1, 1.0), (2, 0.0)],
        );
    }
}

#[test]
fn native_diskann_maintenance_uses_finite_key_only_discovery_and_vacuum() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(&directory.path().join("maintenance.db"), mode);
        let store: Arc<dyn uqa_storage::KeyValueStore> =
            Arc::new(connection.native_diskann_records().unwrap());
        uqa_storage::key_value::conformance::verify_diskann_maintenance(&store).unwrap();
    }
}

#[test]
fn native_diskann_runtime_reclamation_keeps_private_and_committed_readers_through_vacuum() {
    use uqa_storage::key_value::conformance::{
        verify_diskann_reclamation_bounds, verify_diskann_reclamation_reopen,
    };
    for (mode, private) in (0..4).flat_map(|mode| [false, true].map(|private| (mode, private))) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reclamation.db");
        let (first, replacement, partial) = {
            let connection = open(&path, mode);
            let ((first, replacement), held) = retire_and_recreate(&connection, private);
            let control = StorageReadControl::with_limit(1 << 20);
            let repository = connection.diskann_generations(&control).unwrap();
            let physical = repository.open_source(first, &control).unwrap();
            assert!(repository
                .reclaim_retired_step(replacement, 1, &control)
                .is_err());
            assert!(!repository.reclaim_retired_step(first, 1, &control).unwrap());
            connection.vacuum().unwrap();
            scores(&*held, &[(1, 1.0), (2, 0.0)]);
            assert!(repository.open_source(first, &control).is_err());
            let mut complete = false;
            for _ in 0..32 {
                complete = repository.reclaim_retired_step(first, 1, &control).unwrap();
                if complete {
                    break;
                }
            }
            assert!(complete);
            connection.vacuum().unwrap();
            scores(&*held, &[(1, 1.0), (2, 0.0)]);
            physical
                .read_graph_pages(&[0], &control, &mut |_, bytes| {
                    assert_eq!(bytes.len(), 4096);
                    Ok(())
                })
                .unwrap();
            drop(physical);
            drop(held);
            connection.vacuum().unwrap();
            assert_eq!(selected(&connection, &control), replacement);
            let store: Arc<dyn uqa_storage::KeyValueStore> =
                Arc::new(connection.native_diskann_records().unwrap());
            let partial = verify_diskann_reclamation_bounds(&store).unwrap();
            (first, replacement, partial)
        };
        let connection = open(&path, mode);
        let control = StorageReadControl::with_limit(1 << 20);
        let repository = connection.diskann_generations(&control).unwrap();
        assert!(repository.reclaim_retired_step(first, 1, &control).unwrap());
        assert!(repository.resume_stage(first, &control).is_err());
        assert_eq!(selected(&connection, &control), replacement);
        scores(
            &runtime(&connection, &DiskANNTemporaryBudget::new(1 << 20), &control),
            &[(1, 1.0), (2, 0.0)],
        );
        let store: Arc<dyn uqa_storage::KeyValueStore> =
            Arc::new(connection.native_diskann_records().unwrap());
        verify_diskann_reclamation_reopen(&store, partial).unwrap();
        connection
            .with_physical(|sqlite| {
                let retained: i64 = sqlite.query_row(
                    "SELECT count(*) FROM _uqa_mvcc_versions WHERE length(value) >= 32768",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(
                    retained, 0,
                    "final-reader release reclaims large native history payloads"
                );
                Ok(())
            })
            .unwrap();
    }
}
