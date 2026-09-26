//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{
    key_value::conformance::{verify_diskann_restored, verify_diskann_selected_corruption},
    KeyValueStorageBackend, PersistentStorageBackend, PersistentStorageProvider,
};

fn backend(
    connection: &ManagedConnection,
    native: bool,
) -> (Arc<dyn PersistentStorageBackend>, Arc<dyn KeyValueStore>) {
    if native {
        let backend = crate::SQLiteStorageProvider::new(connection.clone())
            .open_session()
            .unwrap()
            .backend;
        (
            backend,
            Arc::new(connection.native_diskann_records().unwrap()),
        )
    } else {
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(connection.clone()).unwrap());
        (Arc::new(KeyValueStorageBackend::new(store.clone())), store)
    }
}

#[test]
fn diskann_selected_corruption_rejects_fresh_queries_and_preserves_retained_sqlite_reads() {
    for mode in 0..4 {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("selected-corruption.db");
            let generation = {
                let connection = connection(&path, mode);
                let (backend, records) = backend(&connection, native);
                verify_diskann_selected_corruption(&*backend, &*records).unwrap()
            };
            let reopened = connection(&path, mode);
            let (backend, _) = backend(&reopened, native);
            verify_diskann_restored(&*backend, generation).unwrap();
        }
    }
}
