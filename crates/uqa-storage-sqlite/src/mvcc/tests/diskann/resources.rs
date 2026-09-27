//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::key_value::conformance::{
    verify_diskann_resource_reopen, verify_diskann_resource_source,
};

#[rstest::rstest]
fn diskann_resources_bound_cold_provider_generations_and_concurrent_queries(
    #[values(false, true)] native: bool,
    #[values(0, 1, 2, 3)] mode: u8,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("resource-acceptance.db");
    let generation = {
        let connection = super::connection(&path, mode);
        let (backend, _records) = super::backend(&connection, native);
        verify_diskann_resource_source(&*backend).unwrap()
    };
    let connection = super::connection(&path, mode);
    let (backend, _records) = super::backend(&connection, native);
    verify_diskann_resource_reopen(&*backend, generation).unwrap();
}
