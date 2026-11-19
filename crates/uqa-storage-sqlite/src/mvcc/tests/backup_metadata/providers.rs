//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Both `SQLite` mappings release physical reads before the next writer or export batch.

use super::*;
use crate::mvcc::native::{NativeRecordFamily, NativeRecordIdentity, NativeRecordOwner};
use rusqlite::types::ValueRef;
use uqa_storage::KeyValueStore;

fn native_key(store: &SQLiteRecordStore, name: &[u8], control: &StorageReadControl) -> Vec<u8> {
    NativeRecordIdentity::new(
        NativeRecordFamily::Schemas,
        NativeRecordOwner::Database(store.native_namespace().unwrap()),
    )
    .unwrap()
    .encode_key(&[ValueRef::Text(name)], control)
    .unwrap()
    .to_vec()
}

#[test]
fn independent_native_and_kv_writers_progress_between_backup_windows_in_all_file_modes() {
    for mode in 0..4 {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("provider.db");
            let connection = super::super::reclamation::open(&path, mode);
            let control = control();
            let (store, before, after) = if native {
                crate::Catalog::open(connection.clone()).unwrap();
                connection
                    .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
                    .unwrap();
                crate::Catalog::open(connection.clone())
                    .unwrap()
                    .save_schema("before_backup")
                    .unwrap();
                let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
                let before = native_key(&store, b"before_backup", &control);
                let after = native_key(&store, b"after_backup", &control);
                (store, before, after)
            } else {
                crate::key_value::SQLiteKeyValueStore::new(connection.clone())
                    .unwrap()
                    .put(b"before_backup", b"first")
                    .unwrap();
                (
                    SQLiteRecordStore::for_key_value(&connection, &control).unwrap(),
                    b"before_backup".to_vec(),
                    b"after_backup".to_vec(),
                )
            };
            observe(&store, b"backup-test", 10, &control);
            let snapshot = capture(&store, &control);
            let original = snapshot
                .records()
                .get(&before, &control)
                .unwrap()
                .unwrap()
                .value()
                .unwrap()
                .to_vec();
            let (send, receive) = std::sync::mpsc::channel();
            let writer_path = path.clone();
            let writer = std::thread::spawn(move || {
                let connection = super::super::reclamation::open(&writer_path, mode);
                let control = super::super::control();
                let store = if native {
                    connection
                        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
                        .unwrap();
                    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
                    crate::Catalog::open(connection)
                        .unwrap()
                        .save_schema("after_backup")
                        .unwrap();
                    store
                } else {
                    crate::key_value::SQLiteKeyValueStore::new(connection.clone())
                        .unwrap()
                        .put(b"after_backup", b"second")
                        .unwrap();
                    SQLiteRecordStore::for_key_value(&connection, &control).unwrap()
                };
                observe(&store, b"backup-test", 20, &control);
                store.reclaim_versions(&control).unwrap();
                send.send(()).unwrap();
            });
            receive
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("writer must finish while backup retains its cut");
            writer.join().unwrap();
            assert_eq!(
                &***snapshot
                    .records()
                    .get(&before, &control)
                    .unwrap()
                    .unwrap()
                    .value()
                    .unwrap(),
                original.as_slice()
            );
            assert!(snapshot.records().get(&after, &control).unwrap().is_none());
            assert!(store
                .snapshot(&control)
                .unwrap()
                .get(&after, &control)
                .unwrap()
                .is_some());
            let watermarks = watermarks(snapshot.as_ref(), &control);
            assert!(watermarks
                .iter()
                .any(|(key, value)| key == b"backup-test" && *value == 10));
            assert_eq!(
                store
                    .identifier_watermark(b"backup-test", &control)
                    .unwrap(),
                Some(20)
            );
        }
    }
}
