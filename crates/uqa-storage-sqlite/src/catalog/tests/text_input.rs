//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ManagedConnection, SQLiteKeyValueStorage, SQLiteStorageProvider};
use uqa_storage::PersistentStorageProvider;

#[test]
fn sqlite_native_analyzer_input_preserves_bindings_labels_and_selected_generations() {
    let provider = SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap());
    uqa_storage::key_value::conformance::verify_catalog_analyzer_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_analyzer_input_preserves_bindings_labels_and_selected_generations() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_catalog_analyzer_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_graph_name_input_preserves_private_membership_and_retirement() {
    let provider = SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap());
    uqa_storage::key_value::conformance::verify_catalog_graph_name_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_graph_name_input_preserves_private_membership_and_retirement() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_catalog_graph_name_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_foreign_table_input_preserves_private_rows_acls_and_retirement() {
    let provider = SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap());
    uqa_storage::key_value::conformance::verify_catalog_foreign_table_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_foreign_table_input_preserves_private_rows_acls_and_retirement() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_catalog_foreign_table_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_schema_input_preserves_private_rows_order_and_retirement() {
    let provider = SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap());
    uqa_storage::key_value::conformance::verify_catalog_schema_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_schema_input_preserves_private_rows_order_and_retirement() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_catalog_schema_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_integer_key_mapping_preserves_markers_and_retained_controls() {
    let provider = SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap());
    uqa_storage::key_value::conformance::verify_integer_key_mapping_input(
        &provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_integer_key_mapping_preserves_markers_and_retained_controls() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_integer_key_mapping_input(
        &provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_table_metadata_preserves_identity_reservations_and_read_limits() {
    let provider = SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap());
    uqa_storage::key_value::conformance::verify_table_metadata_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_table_metadata_preserves_identity_reservations_and_read_limits() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_table_metadata_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_metadata_reads_preserve_private_text_and_original_allowance() {
    let provider = SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap());
    uqa_storage::key_value::conformance::verify_controlled_metadata_reads(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_metadata_reads_preserve_private_text_and_original_allowance() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_controlled_metadata_reads(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_catalog_table_load_assembles_original_private_definitions_and_replacements() {
    let provider = SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap());
    uqa_storage::key_value::conformance::verify_catalog_table_load(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_catalog_table_load_assembles_original_private_definitions_and_replacements() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_catalog_table_load(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_catalog_table_input_retains_private_definitions_and_acl_replacements() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let provider = SQLiteStorageProvider::new(connection);
    uqa_storage::key_value::conformance::verify_catalog_table_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_catalog_table_input_retains_private_definitions_and_acl_replacements() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_catalog_table_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_catalog_index_input_retains_original_definitions_and_private_membership() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let provider = SQLiteStorageProvider::new(connection);
    uqa_storage::key_value::conformance::verify_catalog_index_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_catalog_index_input_retains_original_definitions_and_private_membership() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_catalog_index_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_catalog_text_input_retains_private_rows_and_source_lifetime() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let provider = SQLiteStorageProvider::new(connection);
    uqa_storage::key_value::conformance::verify_catalog_text_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_catalog_text_input_retains_private_rows_and_source_lifetime() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_catalog_text_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_catalog_statistics_input_retains_original_values_and_private_membership() {
    let provider = SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap());
    uqa_storage::key_value::conformance::verify_catalog_statistics_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_catalog_statistics_input_retains_original_values_and_private_membership() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_catalog_statistics_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_native_sequence_input_preserves_definitions_values_order_and_retirement() {
    let provider = SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap());
    uqa_storage::key_value::conformance::verify_catalog_sequence_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn sqlite_key_value_sequence_input_preserves_definitions_values_order_and_retirement() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_catalog_sequence_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}
