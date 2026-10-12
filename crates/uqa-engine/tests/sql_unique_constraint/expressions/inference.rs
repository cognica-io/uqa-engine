//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn expression_index_inference_matches_postgresql(#[case] provider: u8) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("inference.db");
    let open = || {
        if provider == 0 {
            uqa_engine::Engine::new()
        } else {
            super::open_expression_engine(&path, provider - 1)
        }
    };
    let engine = open();
    let mut initial: serde_json::Value =
        serde_json::from_str(include_str!("inference.json")).unwrap();
    let mut reopened = initial.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open()
    };
    reopened["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
}
