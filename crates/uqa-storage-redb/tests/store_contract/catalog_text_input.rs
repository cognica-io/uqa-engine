//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn redb_schema_input_preserves_private_rows_order_and_retirement() {
    let directory = tempfile::tempdir().unwrap();
    let provider = RedbStorage::open(directory.path().join("schema-input.redb")).unwrap();
    uqa_storage::key_value::conformance::verify_catalog_schema_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn redb_integer_key_mapping_preserves_markers_and_retained_controls() {
    let directory = tempfile::tempdir().unwrap();
    let provider = RedbStorage::open(directory.path().join("key-mapping.redb")).unwrap();
    uqa_storage::key_value::conformance::verify_integer_key_mapping_input(
        &provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn redb_table_metadata_preserves_identity_reservations_and_read_limits() {
    let directory = tempfile::tempdir().unwrap();
    let provider = RedbStorage::open(directory.path().join("table-metadata.redb")).unwrap();
    uqa_storage::key_value::conformance::verify_table_metadata_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn redb_metadata_reads_preserve_private_text_and_original_allowance() {
    let directory = tempfile::tempdir().unwrap();
    let provider = RedbStorage::open(directory.path().join("metadata-input.redb")).unwrap();
    uqa_storage::key_value::conformance::verify_controlled_metadata_reads(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn redb_catalog_table_load_assembles_original_private_definitions_and_replacements() {
    let directory = tempfile::tempdir().unwrap();
    let provider = RedbStorage::open(directory.path().join("catalog-table-load.redb")).unwrap();
    uqa_storage::key_value::conformance::verify_catalog_table_load(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn redb_catalog_table_input_retains_private_definitions_and_acl_replacements() {
    let directory = tempfile::tempdir().unwrap();
    let provider = RedbStorage::open(directory.path().join("catalog-table-input.redb")).unwrap();
    uqa_storage::key_value::conformance::verify_catalog_table_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

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

#[test]
fn redb_catalog_statistics_input_retains_original_values_and_private_membership() {
    let directory = tempfile::tempdir().unwrap();
    let provider =
        RedbStorage::open(directory.path().join("catalog-statistics-input.redb")).unwrap();
    uqa_storage::key_value::conformance::verify_catalog_statistics_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}
