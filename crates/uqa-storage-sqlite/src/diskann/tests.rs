//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{path::Path, sync::Arc};

use uqa_core::memory::MemoryError;
use uqa_storage::key_value::conformance::{
    verify_diskann_built_generation, verify_diskann_built_reopen,
};
use uqa_storage::key_value::conformance::{verify_diskann_generations, verify_diskann_reopen};
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{KeyValueStore, StorageBackendError};

use crate::{Catalog, ManagedConnection, SQLiteCompressionOptions, SQLiteKeyValueStore};

fn open(path: &Path, mode: u8) -> ManagedConnection {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "native-diskann-key"),
        2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            "native-diskann-key",
            SQLiteCompressionOptions::default(),
        ),
    }
    .unwrap()
}

fn bind(connection: &ManagedConnection) -> Arc<dyn KeyValueStore> {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    Catalog::open(connection.clone()).unwrap();
    Arc::new(connection.native_diskann_records().unwrap())
}

#[test]
fn native_diskann_maintenance_bounds_sparse_generation_and_mapping_metadata() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(&directory.path().join("retired-metadata.db"), mode);
        let store = bind(&connection);
        let mut previous = None;
        for _ in 0..3 {
            uqa_storage::key_value::conformance::verify_diskann_maintenance(&store).unwrap();
            let counts = connection
                .with_physical(|sqlite| {
                    let mut counts = Vec::new();
                    for table in [
                        "_uqa_mvcc_heads",
                        "_uqa_mvcc_versions",
                        "_uqa_mvcc_runs",
                        "_uqa_mvcc_identifiers",
                    ] {
                        counts.push(sqlite.query_row(
                            &format!("SELECT count(*) FROM {table}"),
                            [],
                            |row| row.get::<_, i64>(0),
                        )?);
                    }
                    Ok(counts)
                })
                .unwrap();
            if let Some(previous) = &previous {
                assert_eq!(
                    &counts, previous,
                    "repeated generation/map cleanup must not retain another physical identity"
                );
            }
            previous = Some(counts);
        }
    }
}

#[test]
fn native_diskann_generations_pass_shared_acceptance_and_cold_reopen_in_all_file_modes() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("generations-{mode}.db"));
        let generation = {
            let connection = open(&path, mode);
            let store = bind(&connection);
            assert_eq!(
                store.auxiliary_encryption_key().is_some(),
                mode == 1 || mode == 3
            );
            let generation = verify_diskann_generations(&store).unwrap();
            connection.with_physical(|sqlite| {
                let pages: i64 = sqlite.query_row("SELECT count(*) FROM _uqa_mvcc_native_diskann_records WHERE typeof(value)='blob' AND length(value)=4096", [], |row| row.get(0))?;
                assert!(pages > 0);
                Ok(())
            }).unwrap();
            generation
        };
        let connection = open(&path, mode);
        let store = bind(&connection);
        verify_diskann_reopen(&store, generation).unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let repository = connection.diskann_generations(&control).unwrap();
        assert!(repository.open_source(generation, &control).is_ok());
    }
}

#[test]
fn native_diskann_bounded_build_seals_and_reopens_complete_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let path = directory.path().join("built.db");
    let (generation, memory_peak, temporary_peak) = {
        let connection = open(&path, 0);
        let store = bind(&connection);
        verify_diskann_built_generation(&store, temporary.path()).unwrap()
    };
    assert!(std::fs::read_dir(temporary.path())
        .unwrap()
        .next()
        .is_none());
    eprintln!(
        "native SQLite DiskANN build: memory={memory_peak}, encrypted temporary={temporary_peak}"
    );
    drop(temporary);
    let connection = open(&path, 0);
    let store = bind(&connection);
    verify_diskann_built_reopen(&store, generation).unwrap();
}

#[test]
fn native_diskann_binary_prefixes_keep_order_empty_keys_and_key_only_limits() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = bind(&connection);
    let keys: &[&[u8]] = &[b"", b"\0", b"\0\0", b"\0\xff", b"\x01", b"\xff"];
    let payload = vec![123; 1 << 18];
    let mut batch = store.batch();
    for key in keys {
        batch.put(key, &payload).unwrap();
    }
    batch.commit().unwrap();
    assert_eq!(store.scan_prefix_keys_after(b"", None, 99).unwrap(), keys);
    let control = StorageReadControl::with_limit(4096);
    store
        .with_read_view(&mut |read| {
            let mut selected = Vec::new();
            read.visit_keys_after(b"\0", Some(b"\0"), 2, &control, &mut |key| {
                selected.push(key.to_vec());
                Ok(())
            })?;
            assert_eq!(selected, [b"\0\0", b"\0\xff"]);
            assert!(read.contains_prefix_budgeted(b"\0", &control)?);
            Ok(())
        })
        .unwrap();
    let mut visited = false;
    let error = store
        .visit_value_bounded(b"\0", 32, &control, &mut |_| {
            visited = true;
            Ok(())
        })
        .unwrap_err();
    assert!(!visited);
    assert!(
        matches!(error, StorageBackendError::Memory(MemoryError::Limit { required, limit: 49 }) if required == payload.len() + 17),
        "{error}"
    );
    assert_eq!(control.memory().used(), 0);
    assert_eq!(store.delete_prefix(b"\0").unwrap(), 3);
    assert_eq!(
        store.scan_prefix_keys_after(b"", None, 99).unwrap(),
        [b"".as_slice(), b"\x01", b"\xff"]
    );
}

#[test]
fn native_diskann_translation_preserves_private_undo_retention_and_atomic_evaluation() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = bind(&connection);
    let peer = store.open_session().unwrap();
    assert_ne!(store.transaction_affinity(), peer.transaction_affinity());
    store.begin_transaction().unwrap();
    store.put(b"point", b"private").unwrap();
    store.savepoint("before").unwrap();
    store.put(b"point", b"replacement").unwrap();
    let retained = store
        .open_retained_read_session(&uqa_core::CancellationToken::new())
        .unwrap();
    store.rollback_to_savepoint("before").unwrap();
    store.release_savepoint("before").unwrap();
    assert_eq!(
        store.get(b"point").unwrap().as_deref(),
        Some(b"private".as_slice())
    );
    assert_eq!(
        retained.get(b"point").unwrap().as_deref(),
        Some(b"replacement".as_slice())
    );
    assert!(peer.get(b"point").unwrap().is_none());
    peer.put(b"other", b"committed").unwrap();
    store.rollback_transaction().unwrap();
    store.vacuum().unwrap();
    assert_eq!(
        retained.get(b"point").unwrap().as_deref(),
        Some(b"replacement".as_slice())
    );
    assert!(retained.put(b"forbidden", b"write").is_err());
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store
            .with_mutation(&mut |_, batch| {
                batch.put(b"failed", b"partial")?;
                panic!("injected native DiskANN evaluation failure");
            })
            .unwrap();
    }));
    assert!(unwind.is_err());
    assert!(!store.in_transaction());
    assert!(store.get(b"failed").unwrap().is_none());
    assert_eq!(
        peer.get(b"other").unwrap().as_deref(),
        Some(b"committed".as_slice())
    );
}

#[test]
fn native_diskann_translation_preserves_versioned_mutation_origins() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = bind(&connection);
    store.begin_transaction().unwrap();
    let mut first = None;
    store
        .with_versioned_mutation(&mut |origin, read, batch| {
            assert!(read.get(b"origin")?.is_none());
            first = Some(origin);
            batch.put(b"origin", &origin.revision().to_le_bytes())
        })
        .unwrap();
    store.savepoint("origin").unwrap();
    let mut undone = None;
    store
        .with_versioned_mutation(&mut |origin, read, batch| {
            assert_eq!(
                read.get(b"origin")?.as_deref(),
                Some(first.unwrap().revision().to_le_bytes().as_slice())
            );
            undone = Some(origin);
            batch.put(b"origin", &origin.revision().to_le_bytes())
        })
        .unwrap();
    store.rollback_to_savepoint("origin").unwrap();
    let mut last = None;
    store
        .with_versioned_mutation(&mut |origin, _, batch| {
            assert_eq!(origin.transaction(), first.unwrap().transaction());
            assert!(origin.revision() > undone.unwrap().revision());
            last = Some(origin);
            batch.put(b"origin", &origin.revision().to_le_bytes())
        })
        .unwrap();
    store.release_savepoint("origin").unwrap();
    store.commit_transaction().unwrap();
    assert_eq!(
        store.get(b"origin").unwrap(),
        Some(last.unwrap().revision().to_le_bytes().to_vec())
    );
    assert!(!store.in_transaction());
}

#[test]
fn native_diskann_entry_requires_native_binding_and_preserves_the_callers_transaction() {
    let control = StorageReadControl::with_limit(1 << 20);
    let connection = ManagedConnection::open_in_memory().unwrap();
    assert!(connection.diskann_generations(&control).is_err());
    let wrong = SQLiteKeyValueStore::open_in_memory().unwrap();
    assert!(wrong.connection().diskann_generations(&control).is_err());
    let store = bind(&connection);
    store.begin_transaction().unwrap();
    store.put(b"caller", b"private").unwrap();
    let repository = connection.diskann_generations(&control).unwrap();
    repository.initialize(&control).unwrap();
    assert!(connection.in_transaction());
    repository.rollback_pending().unwrap();
    assert!(connection.in_transaction());
    connection.rollback_transaction().unwrap();
    assert!(store.get(b"caller").unwrap().is_none());
    assert!(repository.data_identity(&control).is_ok());
}

#[test]
fn native_diskann_compound_retention_preserves_binary_scope_and_private_revision() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = bind(&connection);
    store.begin_transaction().unwrap();
    store.put(b"\0point", b"original").unwrap();
    store.put(b"outside", b"unrelated").unwrap();
    let mut retained = None;
    let mut revision = None;
    let mut record_revision = None;
    store
        .with_read_view(&mut |read| {
            revision = Some(read.revision(&[b"\0"])?);
            record_revision = read.record_revision(b"\0point")?;
            retained = Some(read.retain(&[b"\0"])?);
            Ok(())
        })
        .unwrap();
    let retained = retained.unwrap();
    assert!(revision.as_ref().unwrap().has_private_changes());
    store.put(b"\0point", b"new").unwrap();
    store.rollback_transaction().unwrap();
    drop(store);
    drop(connection);
    assert_eq!(
        retained.get(b"\0point").unwrap().as_deref(),
        Some(b"original".as_slice())
    );
    assert!(retained.revision(&[b"\0"]).unwrap() == revision.unwrap());
    assert!(retained.record_revision(b"\0point").unwrap() == record_revision);
    assert!(retained.record_revision(b"\0missing").unwrap().is_none());
    let control = StorageReadControl::with_limit(4096);
    let mut keys = Vec::new();
    retained
        .visit_keys_after(b"\0", None, 1, &control, &mut |key| {
            keys.push(key.to_vec());
            Ok(())
        })
        .unwrap();
    assert_eq!(keys, [b"\0point"]);
}

#[test]
fn native_diskann_conditions_keep_original_commit_errors_and_reject_stale_evaluation() {
    use std::error::Error;
    use uqa_storage::mvcc::VersionError;

    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = bind(&connection);
    let peer = store.open_session().unwrap();
    store.put(b"guard", b"before").unwrap();
    let error = store
        .with_mutation(&mut |read, batch| {
            assert_eq!(read.get(b"guard")?.as_deref(), Some(b"before".as_slice()));
            peer.put(b"guard", b"after")?;
            batch.require_unchanged(b"guard")?;
            batch.put(b"dependent", b"stale")
        })
        .unwrap_err();
    assert_eq!(error.commit_outcome(), None);
    let mut cause: &(dyn Error + 'static) = &error;
    loop {
        if matches!(
            cause.downcast_ref::<VersionError>(),
            Some(VersionError::ReadConflict { .. })
        ) {
            break;
        }
        cause = cause
            .source()
            .expect("original read conflict must remain typed");
    }
    assert!(store.in_transaction());
    store.rollback_transaction().unwrap();
    assert!(!store.in_transaction());
    assert!(peer.get(b"dependent").unwrap().is_none());
    assert_eq!(
        peer.get(b"guard").unwrap().as_deref(),
        Some(b"after".as_slice())
    );
}

#[test]
fn diskann_build_ownership_protects_live_and_retained_sources() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(&directory.path().join("ownership.db"), mode);
        let store = bind(&connection);
        uqa_storage::key_value::conformance::verify_diskann_build_ownership(&store).unwrap();
    }
}
