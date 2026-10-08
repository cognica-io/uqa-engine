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
fn composite_attribute_types_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("composite-types.db");
    let engine = super::addition::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_attribute_type_oracle.expected.json"
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
    let mut durable = reference;
    durable["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &durable.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_attribute_dependents_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("composite-dependents.db");
    let engine = super::addition::open(provider, &path);
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/composite_attribute_dependency_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_constructor_lifecycle_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constructor-lifecycle.db");
    let engine = super::addition::open(provider, &path);
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/composite_constructor_lifecycle_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_retained_layout_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("retained-layout.db");
    let engine = super::addition::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_retained_layout_oracle.expected.json"
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
    let mut durable = reference;
    durable["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &durable.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_tuple_widths_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tuple-widths.db");
    let engine = super::addition::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_tuple_width_oracle.expected.json"
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
    let mut durable = reference;
    durable["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &durable.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_datum_consumers_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::addition::open(provider, &directory.path().join("datum-consumers.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/composite_datum_consumers_oracle.expected.json"
        ),
    );
}
