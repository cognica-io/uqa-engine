//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Real `SQLite` Key/Value document ownership survives independent connections and closed-file reopen.

use super::*;
use uqa_storage::key_value::conformance::{verify_document_ownership, verify_document_reopen};
use uqa_storage_sqlite::SQLiteKeyValueStore;

#[test]
fn positional_document_projections_keep_snapshots_and_reopen_in_every_file_mode() {
    use uqa_storage::key_value::conformance::{
        verify_document_projections, verify_document_projections_reopen,
    };
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("projections.db");
        {
            let store: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
            verify_document_projections(&store).unwrap();
        }
        let reopened: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
        verify_document_projections_reopen(&reopened).unwrap();
    }
}

#[test]
fn document_owners_coordinate_and_reopen_in_every_file_mode() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("documents.db");
        {
            let a: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
            let b: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
            verify_document_ownership(&a, &b).unwrap();
        }
        let reopened: Arc<dyn KeyValueStore> =
            Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
        verify_document_reopen(&reopened).unwrap();
    }
}
