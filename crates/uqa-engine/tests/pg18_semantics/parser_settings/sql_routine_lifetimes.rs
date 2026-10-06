//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL caller planning, runtime compilation and argument effects match independent references.

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn sql_routine_lifetimes_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::open(provider, &directory.path().join("routine-lifetime.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/sql_caller_plan_lifetime_oracle.expected.json"
        ),
    );
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/routine_parser_lifetime_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn sql_inline_eligibility_and_effects_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::open(provider, &directory.path().join("inline-eligibility.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../../tests/parity/pg18/inline_eligibility_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn strict_null_routine_authority_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::open(provider, &directory.path().join("strict-authority.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/strict_routine_authority_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn sql_body_input_lifetimes_and_invalidation_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::open(provider, &directory.path().join("body-inputs.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/sql_body_input_lifetime_oracle.expected.json"
        ),
    );
}
