//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ManagedConnection, SQLiteKeyValueStorage, SQLiteStorageProvider};
use uqa_storage::PersistentStorageProvider;

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
