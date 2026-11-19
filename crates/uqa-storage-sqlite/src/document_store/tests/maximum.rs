//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{ManagedConnection, SQLiteKeyValueStorage, SQLiteStorageProvider};
use uqa_storage::PersistentStorageProvider;

#[test]
fn native_document_maximum_preserves_private_visibility_and_bounded_progress() {
    let provider = SQLiteStorageProvider::new(ManagedConnection::open_in_memory().unwrap());
    uqa_storage::key_value::conformance::verify_document_maximum_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}

#[test]
fn key_value_document_maximum_preserves_private_visibility_and_bounded_progress() {
    let provider = SQLiteKeyValueStorage::open_in_memory().unwrap();
    uqa_storage::key_value::conformance::verify_document_maximum_input(
        provider.open_session().unwrap(),
    )
    .unwrap();
}
