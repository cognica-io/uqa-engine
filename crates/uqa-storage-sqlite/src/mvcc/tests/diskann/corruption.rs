//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::key_value::conformance::{
    verify_diskann_restored, verify_diskann_selected_corruption,
};

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
