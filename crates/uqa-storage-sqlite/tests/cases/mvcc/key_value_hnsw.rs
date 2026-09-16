//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_storage::key_value::conformance::*;
use uqa_storage::KeyValueStore;
use uqa_storage_sqlite::SQLiteKeyValueStore;

use super::{open, MODES};

#[test]
fn compound_key_value_reads_and_mutations_in_every_sqlite_mode() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("compound.db");
        let a = SQLiteKeyValueStore::new(open(mode, &path)).unwrap();
        let b = SQLiteKeyValueStore::new(open(mode, &path)).unwrap();
        verify_compound_mutations(&a).unwrap();
        verify_compound_concurrency(&a, &b).unwrap();
    }
}

#[test]
fn key_value_hnsw_undo_and_canonical_drift_in_every_sqlite_mode() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SQLiteKeyValueStore::new(open(mode, &directory.path().join("undo.db"))).unwrap(),
        );
        verify_hnsw_undo(store).unwrap();
    }
}

#[test]
fn key_value_hnsw_independent_commits_and_reopen_in_every_sqlite_mode() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hnsw.db");
        {
            let a: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
            let b: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
            verify_hnsw_concurrency(&a, &b).unwrap();
        }
        let reopened = Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
        verify_hnsw_reopen(reopened).unwrap();
    }
}
