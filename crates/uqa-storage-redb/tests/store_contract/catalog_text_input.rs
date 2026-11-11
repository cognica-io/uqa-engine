//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn redb_catalog_index_input_retains_original_definitions_and_private_membership() {
    let directory = tempfile::tempdir().unwrap();
    let provider = RedbStorage::open(directory.path().join("catalog-index-input.redb")).unwrap();
    uqa_storage::key_value::conformance::verify_catalog_index_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn redb_catalog_text_input_retains_private_rows_and_source_lifetime() {
    let directory = tempfile::tempdir().unwrap();
    let provider = RedbStorage::open(directory.path().join("catalog-input.redb")).unwrap();
    uqa_storage::key_value::conformance::verify_catalog_text_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}
