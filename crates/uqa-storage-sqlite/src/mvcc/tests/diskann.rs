//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{path::Path, sync::Arc};

use uqa_storage::key_value::conformance::{
    verify_diskann_built_generation, verify_diskann_built_reopen, verify_diskann_canonical_origins,
    verify_diskann_canonical_reopen, verify_mutation_origins,
};
use uqa_storage::key_value::conformance::{verify_diskann_generations, verify_diskann_reopen};
use uqa_storage::KeyValueStore;

use crate::connection::ManagedConnection;
use crate::key_value::SQLiteKeyValueStore;
use crate::SQLiteCompressionOptions;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod ownership;

#[test]
fn diskann_canonical_reclamation_releases_origins_and_journal_identities_without_a_graph() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let connection = connection(&directory.path().join("canonical-retention.db"), mode);
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection.clone()).unwrap());
        for _ in 0..3 {
            uqa_storage::key_value::conformance::verify_diskann_canonical_reclamation(&store)
                .unwrap();
            connection
                .with_physical(|sqlite| {
                    for table in ["_uqa_mvcc_heads", "_uqa_mvcc_versions"] {
                        let count: i64 = sqlite.query_row(
                            &format!("SELECT count(*) FROM {table} WHERE key >= ?1 AND key < ?2"),
                            rusqlite::params![
                                b"\0uqa-diskann-".as_slice(),
                                b"\0uqa-diskann.".as_slice()
                            ],
                            |row| row.get(0),
                        )?;
                        assert_eq!(
                            count, 0,
                            "cleared canonical/journal identities remain in {table}"
                        );
                    }
                    Ok(())
                })
                .unwrap();
        }
    }
}

#[test]
fn diskann_runtime_reclamation_preserves_sqlite_undo_recreation_and_cold_reopen() {
    use uqa_storage::key_value::conformance::{
        verify_diskann_reclamation_bounds, verify_diskann_reclamation_reopen,
        verify_diskann_runtime_reclaimed_reopen, verify_diskann_runtime_reclamation,
    };
    for (mode, private) in (0..4).flat_map(|mode| [false, true].map(|private| (mode, private))) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reclamation.db");
        let (generations, partial) = {
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
            let generations = verify_diskann_runtime_reclamation(&store, private).unwrap();
            let partial = verify_diskann_reclamation_bounds(&store).unwrap();
            (generations, partial)
        };
        let current = connection(&path, mode);
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(current.clone()).unwrap());
        verify_diskann_runtime_reclaimed_reopen(&store, generations).unwrap();
        verify_diskann_reclamation_reopen(&store, partial).unwrap();
        current
            .with_physical(|sqlite| {
                let retained: i64 = sqlite.query_row(
                    "SELECT count(*) FROM _uqa_mvcc_versions WHERE length(value) >= 32768",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(
                    retained, 0,
                    "final-reader release reclaims large historical payloads"
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn diskann_runtime_retirement_preserves_sqlite_undo_recreation_and_cold_reopen() {
    use uqa_storage::key_value::conformance::{
        verify_diskann_runtime_retirement, verify_diskann_runtime_retirement_reopen,
    };
    for (mode, private) in (0..4).flat_map(|mode| [false, true].map(|private| (mode, private))) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("retirement.db");
        let generations = {
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
            verify_diskann_runtime_retirement(&store, private).unwrap()
        };
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        verify_diskann_runtime_retirement_reopen(&store, generations).unwrap();
    }
}

#[test]
fn diskann_runtime_adoption_rejects_sqlite_ordinal_gaps_and_conflicting_insertions() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(
        SQLiteKeyValueStore::new(connection(&directory.path().join("adoption.db"), 0)).unwrap(),
    );
    uqa_storage::key_value::conformance::verify_diskann_runtime_adoption_conflicts(&store).unwrap();
}

#[test]
fn diskann_runtime_lifecycle_preserves_sqlite_transactions_and_cold_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.db");
        let generation = {
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
            uqa_storage::key_value::conformance::verify_diskann_runtime_lifecycle(&store).unwrap()
        };
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        uqa_storage::key_value::conformance::verify_diskann_runtime_reopen(&store, generation)
            .unwrap();
    }
}

#[test]
fn diskann_live_writes_keep_actual_catalog_visibility_and_sqlite_cold_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live.db");
        let generation = {
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
            uqa_storage::key_value::conformance::verify_diskann_live_writes(&store).unwrap()
        };
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        uqa_storage::key_value::conformance::verify_diskann_live_reopen(&store, generation)
            .unwrap();
    }
}

#[test]
fn diskann_query_views_retain_private_and_old_committed_generations_in_sqlite_modes() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("query-views.db");
        let generation = {
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
            uqa_storage::key_value::conformance::verify_diskann_query_views(&store).unwrap()
        };
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        uqa_storage::key_value::conformance::verify_diskann_query_reopen(&store, generation)
            .unwrap();
    }
}

fn connection(path: &Path, mode: u8) -> ManagedConnection {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "diskann-test-key"),
        2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            "diskann-test-key",
            SQLiteCompressionOptions::default(),
        ),
    }
    .unwrap()
}

#[test]
fn diskann_catalog_identity_handles_survive_sqlite_key_value_cold_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity.db");
        let generation = {
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
            let foreign: Arc<dyn KeyValueStore> = Arc::new(
                SQLiteKeyValueStore::new(connection(&directory.path().join("foreign.db"), mode))
                    .unwrap(),
            );
            uqa_storage::key_value::conformance::verify_diskann_catalog_identity(&store, &foreign)
                .unwrap()
        };
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        uqa_storage::key_value::conformance::verify_diskann_catalog_identity_reopen(
            &store, generation,
        )
        .unwrap();
    }
}

#[test]
fn diskann_publication_is_atomic_and_reopens_in_sqlite_key_value_modes() {
    use uqa_storage::key_value::conformance::{
        verify_diskann_publication, verify_diskann_publication_reopen,
    };
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("publication.db");
        let generation = {
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
            verify_diskann_publication(&store).unwrap()
        };
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        verify_diskann_publication_reopen(&store, generation).unwrap();
    }
}

#[test]
fn diskann_catalog_binding_checks_actual_sqlite_definitions_in_all_file_modes() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(
            SQLiteKeyValueStore::new(connection(&directory.path().join("binding.db"), mode))
                .unwrap(),
        );
        let other: Arc<dyn KeyValueStore> = Arc::new(
            SQLiteKeyValueStore::new(connection(&directory.path().join("foreign.db"), mode))
                .unwrap(),
        );
        uqa_storage::key_value::conformance::verify_diskann_catalog_binding(&store, &other)
            .unwrap();
    }
}

#[test]
fn diskann_canonical_origins_and_tensors_reopen_in_sqlite_key_value_modes() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("origins.db");
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        let origin = verify_diskann_canonical_origins(&store).unwrap();
        drop(store);
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        verify_diskann_canonical_reopen(&store, origin).unwrap();
    }
}

#[test]
fn diskann_mutation_origins_resolve_actual_sqlite_receipts_and_failed_evaluation() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let persistence = Arc::new(super::SQLiteRecordStore::new(&connection).unwrap());
    verify_mutation_origins(persistence).unwrap();
}

#[test]
fn diskann_generations_reopen_through_sqlite_plain_encrypted_and_compressed_owners() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diskann.db");
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        let generation = verify_diskann_generations(&store).unwrap();
        drop(store);
        let reopened: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        verify_diskann_reopen(&reopened, generation).unwrap();
    }
}

#[test]
fn diskann_bounded_build_seals_and_reopens_complete_sqlite_key_value_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let path = directory.path().join("built.db");
    let (generation, memory_peak, temporary_peak) = {
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, 0)).unwrap());
        verify_diskann_built_generation(&store, temporary.path()).unwrap()
    };
    assert!(std::fs::read_dir(temporary.path())
        .unwrap()
        .next()
        .is_none());
    eprintln!("SQLite Key/Value DiskANN build: memory={memory_peak}, encrypted temporary={temporary_peak}");
    drop(temporary);
    let store: Arc<dyn KeyValueStore> =
        Arc::new(SQLiteKeyValueStore::new(connection(&path, 0)).unwrap());
    verify_diskann_built_reopen(&store, generation).unwrap();
}

#[test]
fn diskann_pruning_preserves_late_changes_and_reopens_in_sqlite_modes() {
    use uqa_storage::key_value::conformance::{
        verify_diskann_pruning, verify_diskann_pruning_reopen,
    };
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pruning.db");
        let generation = {
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
            verify_diskann_pruning(&store).unwrap()
        };
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        verify_diskann_pruning_reopen(&store, generation).unwrap();
    }
}

#[test]
fn diskann_maintenance_source_preserves_census_builds_and_reopens_in_sqlite_modes() {
    use uqa_storage::key_value::{
        conformance::{verify_diskann_maintenance_reopen, verify_diskann_maintenance_source},
        KeyValueStorageBackend,
    };
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("maintenance-source.db");
        let generation = {
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
            verify_diskann_maintenance_source(&KeyValueStorageBackend::new(store)).unwrap()
        };
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection(&path, mode)).unwrap());
        verify_diskann_maintenance_reopen(&KeyValueStorageBackend::new(store), generation).unwrap();
    }
}

#[test]
fn diskann_build_ownership_protects_live_and_retained_sources() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn KeyValueStore> = Arc::new(
            SQLiteKeyValueStore::new(connection(&directory.path().join("ownership.db"), mode))
                .unwrap(),
        );
        uqa_storage::key_value::conformance::verify_diskann_build_ownership(&store).unwrap();
        uqa_storage::key_value::conformance::verify_diskann_publication_ownership(&store).unwrap();
    }
}

#[test]
fn diskann_maintenance_uses_finite_key_only_discovery_and_vacuum() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let connection = connection(&directory.path().join("maintenance.db"), mode);
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection.clone()).unwrap());
        for _ in 0..3 {
            uqa_storage::key_value::conformance::verify_diskann_maintenance(&store).unwrap();
            connection.with_physical(|sqlite| {
                for (table, expected) in [("_uqa_mvcc_heads", 2), ("_uqa_mvcc_versions", 1), ("_uqa_mvcc_runs", 0)] {
                    assert_eq!(sqlite.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get::<_, i64>(0))?, expected, "only the data marker and unrelated fixture tombstone remain in {table}");
                }
                Ok(())
            }).unwrap();
        }
    }
}
