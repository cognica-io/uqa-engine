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
fn composite_tuple_boundary_prefix_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tuple-boundary.db");
    let engine = super::addition::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_tuple_boundary_oracle.expected.json"
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
fn composite_tuple_boundary_reads_reject_missing_bytes(#[case] provider: usize) {
    fn execute(engine: &uqa_engine::Engine, sql: &str) -> serde_json::Value {
        let result = crate::pg18_oracle::run_case(engine, sql);
        assert!(result["error"].is_null(), "{sql}: {result}");
        result
    }

    fn reject(engine: &uqa_engine::Engine, sql: &str) {
        let result = crate::pg18_oracle::run_case(engine, sql);
        assert_eq!(result["error"]["sqlstate"], "XX001", "{sql}: {result}");
        assert_eq!(result["error"]["message"], "invalid datum length");
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tuple-boundary-errors.db");
    let engine = super::addition::open(provider, &path);
    execute(
        &engine,
        "CREATE TYPE boundary_one AS (a integer); CREATE VIEW boundary_value AS SELECT '(-1)'::boundary_one AS v; PREPARE boundary_whole AS SELECT '(-1)'::boundary_one AS v; EXECUTE boundary_whole; ALTER TYPE boundary_one ALTER ATTRIBUTE a TYPE bigint",
    );
    for sql in [
        "SELECT v FROM boundary_value",
        "SELECT v::text FROM boundary_value",
        "SELECT (v).a FROM boundary_value",
        "SELECT (v).a = 0 FROM boundary_value",
        "EXECUTE boundary_whole",
    ] {
        reject(&engine, sql);
    }
    execute(&engine, "BEGIN; SAVEPOINT before_read");
    reject(&engine, "SELECT v::text FROM boundary_value");
    execute(
        &engine,
        "ROLLBACK TO before_read; ALTER TYPE boundary_one ALTER ATTRIBUTE a TYPE integer",
    );
    let result = execute(&engine, "SELECT v::text FROM boundary_value");
    assert_eq!(result["results"][0]["rows"], serde_json::json!([["(-1)"]]));
    execute(&engine, "ROLLBACK");
    reject(&engine, "SELECT v::text FROM boundary_value");
    execute(
        &engine,
        "ALTER TYPE boundary_one ALTER ATTRIBUTE a TYPE double precision",
    );
    reject(&engine, "SELECT v::text FROM boundary_value");

    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        super::addition::open(provider, &path)
    };
    reject(&engine, "SELECT v::text FROM boundary_value");
    execute(
        &engine,
        "ALTER TYPE boundary_one ALTER ATTRIBUTE a TYPE integer",
    );
    let result = execute(&engine, "SELECT v::text FROM boundary_value");
    assert_eq!(result["results"][0]["rows"], serde_json::json!([["(-1)"]]));
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn peer_tuple_projection_preserves_prepared_view_constants(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tuple-boundary-peer.db");
    let engine = super::addition::open(provider, &path);
    let peer = engine.new_session().unwrap();
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_tuple_boundary_oracle.expected.json"
    ))
    .unwrap();
    let verify = |engine: &uqa_engine::Engine, id: &str| {
        let case = reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == id)
            .unwrap();
        let mut selected = reference.clone();
        selected["cases"] = serde_json::json!([case]);
        crate::pg18_oracle::verify(engine, &selected.to_string());
    };
    verify(&engine, "setup");
    verify(&peer, "prepare");
    verify(&engine, "widen");
    verify(&peer, "changed_prepared_result");
    verify(&peer, "prefix");
    engine
        .sql("CREATE OR REPLACE VIEW boundary_prefix_value AS SELECT '(7,8,9)'::boundary_prefix AS v", &[])
        .unwrap();
    let result = crate::pg18_oracle::run_case(&peer, "EXECUTE boundary_fields");
    assert_eq!(result["error"]["sqlstate"], "0A000");
    assert_eq!(
        result["error"]["message"],
        "cached plan must not change result type"
    );
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
    verify_reopened_oracle(
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
    verify_reopened_oracle(
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
    verify_reopened_oracle(
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
    verify_reopened_oracle(
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
    verify_reopened_oracle(
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
    verify_reopened_oracle(
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
    verify_reopened_oracle(
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

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn physical_record_descriptors_match_postgresql(#[case] provider: usize) {
    verify_reopened_oracle(
        provider,
        include_str!(
            "../../../../../tests/parity/pg18/composite_physical_record_oracle.expected.json"
        ),
    );
}

fn verify_reopened_oracle(provider: usize, reference: &str) {
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
