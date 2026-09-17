//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Identifier reservations coordinate separate `SQLite` connections without publishing their private logical records.

use super::*;
use std::num::NonZeroU64;
use uqa_storage::PersistentStorageBackend;
use uqa_storage_sqlite::{Catalog, SQLiteKeyValueStore, SQLiteStorageBackend};

#[test]
fn identifier_batches_are_forwarded_and_reopen_in_every_file_mode() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identifier-batches.db");
        let last = {
            let a = SQLiteKeyValueStore::new(open(mode, &path)).unwrap();
            let b = SQLiteKeyValueStore::new(open(mode, &path)).unwrap();
            uqa_storage::mvcc::verify_identifier_batches(&a, &b).unwrap()
        };
        let reopened = SQLiteKeyValueStore::new(open(mode, &path)).unwrap();
        assert_eq!(
            reopened
                .identifier_allocator()
                .unwrap()
                .identifier_watermark(b"identifier-batches")
                .unwrap(),
            Some(last)
        );
        assert_eq!(
            reopened
                .identifier_allocator()
                .unwrap()
                .allocate_identifiers(b"identifier-batches", request(1, 1))
                .unwrap()
                .watermark(),
            last + 1
        );
    }
}

#[test]
fn document_id_backends_reserve_independently_and_reopen_in_every_file_mode() {
    use uqa_storage::document_store::identifiers::{
        conformance::verify_document_id_sessions, DocumentIdAllocator,
    };
    use uqa_storage::{KeyValueCatalog, KeyValueStorageBackend, PersistentStorageSession};
    for mode in MODES {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("document-identifiers.db");
            let pair = |initialize| {
                let connection = open(mode, &path);
                if native {
                    if initialize {
                        Catalog::open(connection.clone()).unwrap();
                    }
                    connection
                        .bind_native_records(VersionedSessionOptions::default())
                        .unwrap();
                    PersistentStorageSession::new(
                        Arc::new(Catalog::open(connection.clone()).unwrap()),
                        Arc::new(SQLiteStorageBackend::new(connection)),
                    )
                } else {
                    let store: Arc<dyn KeyValueStore> =
                        Arc::new(SQLiteKeyValueStore::new(connection).unwrap());
                    PersistentStorageSession::new(
                        Arc::new(KeyValueCatalog::new(store.clone())),
                        Arc::new(KeyValueStorageBackend::new(store)),
                    )
                }
            };
            let last = {
                let a = pair(true);
                let b = pair(false);
                verify_document_id_sessions(&a, &b).unwrap()
            };
            let reopened = pair(false);
            let ids = DocumentIdAllocator::new(
                reopened.backend.identifier_allocator(),
                [11; 16],
                [12; 16],
            )
            .unwrap();
            assert_eq!(ids.allocate(&mut 1).unwrap(), last + 1);
        }
    }
}

fn request(minimum: u64, count: u64) -> IdentifierRequest {
    IdentifierRequest::Reserve {
        minimum,
        maximum: u64::MAX,
        count: NonZeroU64::new(count).unwrap(),
    }
}

fn records(mode: Mode, path: &Path, layout: u8, initialize: bool) -> SQLiteRecordStore {
    let connection = open(mode, path);
    if layout == 2 {
        if initialize {
            Catalog::open(connection.clone()).unwrap();
        }
        SQLiteRecordStore::for_native(&connection, &StorageReadControl::with_limit(1 << 20))
            .unwrap()
    } else {
        if layout == 1 {
            SQLiteKeyValueStore::new(connection.clone()).unwrap();
        }
        SQLiteRecordStore::new(&connection).unwrap()
    }
}

#[test]
fn identifier_reservations_coordinate_all_sqlite_layouts_and_reopen_in_every_file_mode() {
    let control = StorageReadControl::with_limit(1 << 20);
    for mode in MODES {
        for layout in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("identifiers.db");
            let identity = {
                let a = records(mode, &path, layout, true);
                let b = records(mode, &path, layout, false);
                verify_identifier_allocations(&a, &b, &control).unwrap();
                assert_eq!(
                    a.allocate_identifiers(b"reopen", request(41, 3), &control)
                        .unwrap()
                        .range(),
                    Some(41..=43)
                );
                a.database_id()
            };
            let reopened = records(mode, &path, layout, false);
            assert_eq!(reopened.database_id(), identity);
            assert_eq!(
                reopened.identifier_watermark(b"reopen", &control).unwrap(),
                Some(43)
            );
            assert_eq!(
                reopened
                    .allocate_identifiers(b"reopen", request(0, 1), &control)
                    .unwrap()
                    .range(),
                Some(44..=44)
            );
        }
    }
}

#[test]
fn native_identifier_reservations_do_not_publish_or_undo_private_catalog_changes() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("native-private-identifiers.db");
        let a = open(mode, &path);
        Catalog::open(a.clone()).unwrap();
        a.bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let records =
            SQLiteRecordStore::for_native(&a, &StorageReadControl::with_limit(1 << 20)).unwrap();
        let catalog = Catalog::open(a.clone()).unwrap();
        let backend = SQLiteStorageBackend::new(a.clone());
        let b = open(mode, &path);
        b.bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let observer = Catalog::open(b.clone()).unwrap();
        let other =
            SQLiteRecordStore::for_native(&b, &StorageReadControl::with_limit(1 << 20)).unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        backend.begin_transaction().unwrap();
        catalog
            .set_metadata("private-identifier-fixture", "uncommitted")
            .unwrap();
        let checkpoint = uqa_storage::StorageSavepointId::allocate();
        backend.savepoint(checkpoint).unwrap();
        assert_eq!(
            records
                .allocate_identifiers(b"entities", request(1, 2), &control)
                .unwrap()
                .range(),
            Some(1..=2)
        );
        assert_eq!(
            other
                .allocate_identifiers(b"entities", request(1, 2), &control)
                .unwrap()
                .range(),
            Some(3..=4)
        );
        assert!(observer
            .get_metadata("private-identifier-fixture")
            .unwrap()
            .is_none());
        assert_eq!(
            backend
                .identifier_allocator()
                .unwrap()
                .identifier_watermark(b"entities")
                .unwrap(),
            Some(4)
        );
        assert!(backend.in_transaction());
        backend.rollback_to_savepoint(checkpoint).unwrap();
        backend.rollback_transaction().unwrap();
        assert!(catalog
            .get_metadata("private-identifier-fixture")
            .unwrap()
            .is_none());
        assert_eq!(
            records
                .allocate_identifiers(b"entities", request(1, 1), &control)
                .unwrap()
                .range(),
            Some(5..=5)
        );
    }
}

#[cfg(not(target_os = "emscripten"))]
#[test]
fn identifier_ranges_remain_disjoint_across_native_processes() {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let control = StorageReadControl::with_limit(1 << 20);
    for (mode_index, mode) in MODES.into_iter().enumerate() {
        for layout in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("process-identifiers.db");
            let parent = records(mode, &path, layout, true);
            assert_eq!(
                parent
                    .allocate_identifiers(b"process-entities", request(1, 2), &control)
                    .unwrap()
                    .range(),
                Some(1..=2)
            );
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "mvcc::process_writer_helper",
                    "--ignored",
                    "--nocapture",
                ])
                .env("UQA_SQLITE_RECORD_TEST_FILE", &path)
                .env("UQA_SQLITE_RECORD_TEST_MODE", mode_index.to_string())
                .env("UQA_SQLITE_IDENTIFIER_TEST_LAYOUT", layout.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    let output = child.wait_with_output().unwrap();
                    panic!(
                        "identifier allocation child timed out: {mode:?} {layout}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("identifier-allocation-complete")
            );
            assert_eq!(
                parent
                    .allocate_identifiers(b"process-entities", request(1, 1), &control)
                    .unwrap()
                    .range(),
                Some(5..=5)
            );
        }
    }
}

#[cfg(not(target_os = "emscripten"))]
pub(super) fn process_helper(mode: Mode, path: &Path, layout: u8) {
    let store = records(mode, path, layout, false);
    let control = StorageReadControl::with_limit(1 << 20);
    assert_eq!(
        store
            .allocate_identifiers(b"process-entities", request(1, 2), &control)
            .unwrap()
            .range(),
        Some(3..=4)
    );
    println!("identifier-allocation-complete");
}
