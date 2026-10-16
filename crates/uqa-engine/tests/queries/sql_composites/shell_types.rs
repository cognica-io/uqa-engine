//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Undefined type identity, completion, ACLs and transaction lifetime against `PostgreSQL`.

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn shell_type_catalog_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shell-types.db");
    let engine = super::addition::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/shell_type_catalog_oracle.expected.json"
    ))
    .unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        super::addition::open(provider, &path)
    };
    let mut reopened = reference;
    reopened["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn shell_type_sessions_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shell-sessions.db");
    let engine = super::addition::open(provider, &path);
    let writer = engine.new_session().unwrap();
    let peer = engine.new_session().unwrap();
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/shell_type_sessions_oracle.expected.json"
    ))
    .unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let session = if case["session"] == "writer" {
            &writer
        } else {
            &peer
        };
        let mut one = reference.clone();
        one["cases"] = serde_json::json!([case]);
        crate::pg18_oracle::verify(session, &one.to_string());
    }
    drop(writer);
    drop(peer);
    drop(engine);
    let reopened = super::addition::open(provider, &path);
    let mut observation = reference.clone();
    observation["cases"] =
        serde_json::json!([reference["cases"].as_array().unwrap().last().unwrap()]);
    crate::pg18_oracle::verify(&reopened, &observation.to_string());
}
