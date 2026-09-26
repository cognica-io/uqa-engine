//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::open;
use crate::SQLiteStorageProvider;
use uqa_storage::{
    key_value::conformance::{
        verify_diskann_maintenance_reopen, verify_diskann_maintenance_source,
    },
    PersistentStorageProvider,
};

#[test]
fn native_diskann_maintenance_source_preserves_census_builds_and_cold_reopen() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("maintenance-source.db");
        let generation = {
            let provider = SQLiteStorageProvider::new(open(&path, mode));
            let session = provider.open_session().unwrap();
            verify_diskann_maintenance_source(&*session.backend).unwrap()
        };
        let provider = SQLiteStorageProvider::new(open(&path, mode));
        let session = provider.open_session().unwrap();
        verify_diskann_maintenance_reopen(&*session.backend, generation).unwrap();
    }
}
