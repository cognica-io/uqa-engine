//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent and correlated FROM evaluation across native providers.

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn lateral_function_inputs_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::values::open(provider, &directory.path().join("lateral-functions.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/lateral_functions_oracle.expected.json"),
    );
}
