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
