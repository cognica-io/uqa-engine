//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common sequence consumers preserve their chosen `SQLite` session through conflicts and reopen.

use std::sync::Arc;
use uqa_storage::key_value::conformance::{verify_sequence_concurrency, verify_sequence_reopen};
use uqa_storage::KeyValueStore;
use uqa_storage_sqlite::SQLiteKeyValueStore;

use super::{open, MODES};

#[test]
fn sequence_consumers_keep_conflicts_undo_and_reopen_in_every_sqlite_mode() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sequences.db");
        {
            let a: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
            let b: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
            verify_sequence_concurrency(&a, &b).unwrap();
        }
        let store: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
        verify_sequence_reopen(&store).unwrap();
    }
}
