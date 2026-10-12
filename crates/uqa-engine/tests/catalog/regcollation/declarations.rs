//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared collation identities survive catalog inheritance, alteration and persistent reopen.

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn declared_collation_metadata_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("declared-collations.db");
    let engine = super::open(provider, &path);
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("declarations.json")).unwrap();
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
        super::open(provider, &path)
    };
    let mut durable = reference;
    durable["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &durable.to_string());
}
