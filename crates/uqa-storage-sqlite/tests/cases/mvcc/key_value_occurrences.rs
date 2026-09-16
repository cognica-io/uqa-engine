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
fn key_value_occurrence_snapshots_independent_commits_and_reopen_in_every_mode() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("occurrences.db");
        {
            let a: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
            let b: Arc<dyn KeyValueStore> =
                Arc::new(SQLiteKeyValueStore::new(open(mode, &path)).unwrap());
            verify_occurrence_snapshots(&a).unwrap();
            verify_occurrence_concurrency(&a, &b).unwrap();
            verify_occurrence_accelerators(&a, &b).unwrap();
        }
        verify_occurrence_reopen(Arc::new(
            SQLiteKeyValueStore::new(open(mode, &path)).unwrap(),
        ))
        .unwrap();
    }
}
