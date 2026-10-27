//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn generic_explain_and_prepared_parameter_hooks_match_postgresql(#[case] provider: Provider) {
    let reference = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_generic.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-generic.db"));
    super::reference::verify(&engine, &reference);
}
