//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::range_input::open;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn drop_owned_matches_postgresql(#[case] provider: usize) {
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("drop-owned.db");
    let engine = open(provider, &path);
    let transcript = include_str!("../../../../tests/parity/pg18/drop_owned_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider != 0 {
        drop(engine);
        let engine = open(provider, &path);
        let mut transcript: serde_json::Value = serde_json::from_str(transcript).unwrap();
        transcript["cases"].as_array_mut().unwrap().retain(|case| case["reopen"] == true);
        crate::pg18_oracle::verify(&engine, &transcript.to_string());
    }
}
