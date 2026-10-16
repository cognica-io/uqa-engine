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

fn verify_consumers(provider: usize, family: Option<&str>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("composite-consumers.db");
    let engine = super::addition::open(provider, &path);
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
