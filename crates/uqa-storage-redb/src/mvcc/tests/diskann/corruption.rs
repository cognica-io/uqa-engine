//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::{
    key_value::conformance::{verify_diskann_restored, verify_diskann_selected_corruption},
    PersistentStorageProvider,
};

#[test]
fn diskann_selected_corruption_rejects_fresh_queries_and_preserves_retained_redb_reads() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("selected-corruption.redb");
    let generation = {
        let owner = crate::RedbStorage::open(&path).unwrap();
        let session = owner.open_session().unwrap();
        verify_diskann_selected_corruption(&*session.backend, &owner.store()).unwrap()
    };
    let owner = crate::RedbStorage::open(&path).unwrap();
    let session = owner.open_session().unwrap();
    verify_diskann_restored(&*session.backend, generation).unwrap();
}
