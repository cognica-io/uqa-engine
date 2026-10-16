//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite field families preserve equality classes across relational and index consumers.

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_consumer_matrix_matches_postgresql(#[case] provider: usize) {
    verify_consumers(provider, None);
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn bpchar_composite_output_and_consumers_match_postgresql(#[case] provider: usize) {
    verify_consumers(provider, Some("bpchar_"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn range_composite_consumers_match_postgresql(#[case] provider: usize) {
    verify_reference(
        provider,
        serde_json::from_str(include_str!(
            "../../../../../tests/parity/pg18/composite_range_consumers_oracle.expected.json"
        ))
        .unwrap(),
    );
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn range_unique_reservations_follow_postgresql_across_sessions(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("range-unique.db");
    let engine = super::addition::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_range_reservations_oracle.json"
    ))
    .unwrap();
    let holder = engine.new_session().unwrap();
    let contender = engine.new_session().unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let session = match case["session"].as_str().unwrap() {
            "holder" => &holder,
            "contender" => &contender,
            other => panic!("unknown reference session {other}"),
        };
        let sql = case["sql"].as_str().unwrap();
        let actual = crate::pg18_oracle::run_case(session, sql);
        assert_eq!(
            actual["error"]["sqlstate"], case["sqlstate"],
            "{sql}: {actual}"
        );
        if case["sqlstate"].is_null() {
            assert_eq!(
                actual["command_tags"],
                serde_json::json!([case["command_tag"]]),
                "{sql}: {actual}"
            );
        } else {
            assert_eq!(
                actual["error"]["message"], case["message"],
                "{sql}: {actual}"
            );
        }
    }
}

fn verify_consumers(provider: usize, family: Option<&str>) {
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_consumer_matrix_oracle.expected.json"
    ))
    .unwrap();
    if let Some(family) = family {
        reference["cases"].as_array_mut().unwrap().retain(|case| {
            let id = case["id"].as_str().unwrap();
            id == "version" || id.starts_with(family)
        });
    }
    verify_reference(provider, reference);
}

fn verify_reference(provider: usize, reference: serde_json::Value) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("composite-consumers.db");
    let engine = super::addition::open(provider, &path);
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
    let mut durable = reference;
    durable["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &durable.to_string());
}
