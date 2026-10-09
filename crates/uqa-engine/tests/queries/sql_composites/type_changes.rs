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

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn nested_composite_layouts_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::addition::open(provider, &directory.path().join("nested-layouts.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/composite_nested_layout_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_enum_layouts_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("enum-layouts.db");
    let engine = super::addition::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_enum_layout_oracle.expected.json"
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
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn legacy_enum_constants_fill_physical_oids_before_type_changes(#[case] provider: usize) {
    fn remove_oids(value: &mut serde_json::Value) -> usize {
        match value {
            serde_json::Value::Array(values) => values.iter_mut().map(remove_oids).sum(),
            serde_json::Value::Object(fields) => {
                let removed = usize::from(
                    fields.get("$uqa_type").is_some_and(|kind| kind == "enum")
                        && fields.remove("label_oid").is_some(),
                );
                removed + fields.values_mut().map(remove_oids).sum::<usize>()
            }
            _ => 0,
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-enum-layouts.db");
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_enum_layout_oracle.expected.json"
    ))
    .unwrap();
    let statement = |id| {
        reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == id)
            .unwrap()["sql"]
            .as_str()
            .unwrap()
    };
    let engine = super::addition::open(provider, &path);
    engine.sql(statement("setup"), &[]).unwrap();
    // Exercise both an already retained source and a constant captured by its first TYPE change.
    engine
        .sql("ALTER TYPE enum_slots ADD ATTRIBUTE extra integer", &[])
        .unwrap();
    drop(engine);
    super::addition::restoration::catalog(provider, &path, |catalog| {
        let mut count = 0;
        for mut view in catalog.load_views().unwrap() {
            let mut value = serde_json::from_str(&view.definition_json).unwrap();
            count += remove_oids(&mut value);
            view.definition_json = serde_json::to_string(&value).unwrap();
            catalog.save_view(&view).unwrap();
        }
        assert!(
            count >= 3,
            "legacy scalar, retained source and array carriers"
        );
    });
    let engine = super::addition::open(provider, &path);
    engine.sql(statement("commit_changes"), &[]).unwrap();
    let mut observations = reference;
    observations["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| matches!(case["id"].as_str(), Some("scalar_oid" | "array_label_oids")));
    crate::pg18_oracle::verify(&engine, &observations.to_string());
    drop(engine);
    let reopened = super::addition::open(provider, &path);
    crate::pg18_oracle::verify(&reopened, &observations.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_enum_reads_match_postgresql(#[case] provider: usize) {
    verify_enum_oracle(
        provider,
        include_str!("../../../../../tests/parity/pg18/composite_enum_read_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_enum_support_functions_match_postgresql(#[case] provider: usize) {
    verify_enum_oracle(
        provider,
        include_str!(
            "../../../../../tests/parity/pg18/composite_enum_support_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_enum_call_state_matches_postgresql(#[case] provider: usize) {
    verify_enum_oracle(
        provider,
        include_str!(
            "../../../../../tests/parity/pg18/composite_enum_call_state_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_enum_operators_match_postgresql(#[case] provider: usize) {
    verify_enum_oracle(
        provider,
        include_str!(
            "../../../../../tests/parity/pg18/composite_enum_operator_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_enum_consumers_match_postgresql(#[case] provider: usize) {
    verify_enum_oracle(
        provider,
        include_str!(
            "../../../../../tests/parity/pg18/composite_enum_consumer_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_nested_enum_equality_matches_postgresql(#[case] provider: usize) {
    verify_enum_oracle(
        provider,
        include_str!(
            "../../../../../tests/parity/pg18/composite_enum_nested_equality_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_nested_enum_order_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("enum-order.db");
    let engine = super::addition::open(provider, &path);
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/composite_enum_nested_order_oracle.expected.json"
        ),
    );
    let cold = include_str!(
        "../../../../../tests/parity/pg18/composite_enum_nested_order_session_oracle.expected.json"
    );
    let mut retained: serde_json::Value = serde_json::from_str(cold).unwrap();
    retained["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["id"] != "cleanup");
    if provider == 0 {
        let fresh = super::addition::open(provider, &path);
        let mut setup: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../tests/parity/pg18/composite_enum_nested_order_oracle.expected.json"
        ))
        .unwrap();
        setup["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| case["id"] == "setup");
        crate::pg18_oracle::verify(&fresh, &setup.to_string());
        crate::pg18_oracle::verify(&fresh, cold);
        return;
    }
    let fresh = engine.new_session().unwrap();
    crate::pg18_oracle::verify(&fresh, &retained.to_string());
    drop(fresh);
    drop(engine);
    let engine = super::addition::open(provider, &path);
    crate::pg18_oracle::verify(&engine, cold);
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn generated_enum_functions_match_postgresql(#[case] provider: usize) {
    verify_enum_oracle(
        provider,
        include_str!("../../../../../tests/parity/pg18/enum_generated_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn retained_enum_sort_consumers_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::addition::open(provider, &directory.path().join("enum-sort.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../../tests/parity/pg18/composite_enum_sort_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn retained_enum_set_consumers_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::addition::open(provider, &directory.path().join("enum-set.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../../tests/parity/pg18/composite_enum_set_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn retained_enum_window_consumers_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::addition::open(provider, &directory.path().join("enum-window.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../../tests/parity/pg18/composite_enum_window_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn retained_enum_aggregate_consumers_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::addition::open(provider, &directory.path().join("enum-aggregate.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/composite_enum_aggregate_oracle.expected.json"
        ),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn physical_catalog_arrays_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::addition::open(provider, &directory.path().join("catalog-arrays.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../../tests/parity/pg18/composite_catalog_array_oracle.expected.json"
        ),
    );
}

fn verify_enum_oracle(provider: usize, reference: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("enum-reads.db");
    let engine = super::addition::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(reference).unwrap();
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
