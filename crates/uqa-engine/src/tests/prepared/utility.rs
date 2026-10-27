//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;
use serde_json::Value as Json;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn prepared_session_utilities_match_postgresql(#[case] provider: Provider) {
    let reference: Json = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/prepared_utility_oracle.expected.json"
    ))
    .unwrap();
    assert!(reference["postgresql_version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18."));
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("utility.db"));
    super::reference::verify(&engine, &reference);
}
