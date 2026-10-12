//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::open;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn query_record_descriptors_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("query-records.db");
    let engine = open(provider, &path);
    let mut initial: serde_json::Value =
        serde_json::from_str(include_str!("query_records.json")).unwrap();
    let mut reopened = initial.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    if provider != 0 {
        drop(engine);
        let engine = open(provider, &path);
        reopened["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| case["reopen"] == true);
        crate::pg18_oracle::verify(&engine, &reopened.to_string());
    }
}
