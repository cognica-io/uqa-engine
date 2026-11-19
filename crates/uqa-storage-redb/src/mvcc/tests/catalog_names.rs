//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::PersistentStorageProvider;

#[test]
fn redb_graph_name_input_preserves_private_membership_and_retirement() {
    let directory = tempfile::tempdir().unwrap();
    let provider = crate::RedbStorage::open(directory.path().join("graph-names.redb")).unwrap();
    uqa_storage::key_value::conformance::verify_catalog_graph_name_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}
