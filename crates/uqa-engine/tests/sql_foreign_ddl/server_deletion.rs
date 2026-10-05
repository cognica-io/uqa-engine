//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Server deletion, role authority and retained dependents against independent `PostgreSQL` results.

use super::declarations::open;

const ORACLE: &str =
    include_str!("../../../../tests/parity/pg18/drop_foreign_server_oracle.expected.json");
const ROUTINES: &str =
    include_str!("../../../../tests/parity/pg18/drop_foreign_server_routine_oracle.expected.json");

fn select_cases(
    oracle: &serde_json::Value,
    selected: impl FnMut(&serde_json::Value) -> bool,
) -> String {
    let mut transcript = oracle.clone();
    transcript["cases"].as_array_mut().unwrap().retain(selected);
    transcript.to_string()
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn foreign_server_deletion_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("drop-foreign-servers.db");
    let engine = open(provider, &path);
    let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    crate::pg18_oracle::verify(
        &engine,
        &select_cases(&oracle, |case| case["final_cleanup"] != true),
    );
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        let reopened = open(provider, &path);
        crate::pg18_oracle::verify(
            &reopened,
            &select_cases(&oracle, |case| case["reopen"] == true),
        );
        reopened
    };
    crate::pg18_oracle::verify(
        &engine,
        &select_cases(&oracle, |case| case["final_cleanup"] == true),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn foreign_server_drop_routines_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("drop-foreign-server-routines.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, ROUTINES);
}
