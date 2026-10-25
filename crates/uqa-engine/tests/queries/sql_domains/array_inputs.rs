//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` array input timing, domain constraints and retained input effects.

use uqa_engine::Engine;

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn array_valued_domains_remain_scalar_elements(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("array-value-inputs.db");
    let engine = open(provider, &path);
    engine
        .sql(
            "CREATE DOMAIN array_leaf AS integer[]; CREATE TABLE array_values(v array_leaf[])",
            &[],
        )
        .unwrap();
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("array_value_inputs.pg18.json")).unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let actual = crate::pg18_oracle::run_case(&engine, case["sql"].as_str().unwrap());
        assert_eq!(actual["error"], serde_json::Value::Null, "{case}");
        assert_eq!(actual["results"][0]["rows"], case["rows"], "{case}");
    }
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let first = engine
        .create_typed_parameter_with_control("array_leaf", Some("[4:5]={7,8}"), &control)
        .unwrap();
    let second = engine
        .create_typed_parameter_with_control("array_leaf", Some("{}"), &control)
        .unwrap();
    let elements = [
        first.into_uncontrolled().unwrap(),
        second.into_uncontrolled().unwrap(),
    ];
    let array = engine
        .create_array_parameter_with_control("array_leaf[]", &[2], &[-2], &elements, &control)
        .unwrap();
    engine
        .sql(
            "INSERT INTO array_values VALUES ($1)",
            &[array.into_uncontrolled().unwrap()],
        )
        .unwrap();
    let query = "SELECT v::text, array_dims(v), cardinality(v)::text FROM array_values";
    let mut decoded = engine.sql("SELECT v FROM array_values", &[]).unwrap();
    engine.render_enum_labels(&mut decoded).unwrap();
    let uqa_core::Value::Array(array) = &decoded.rows[0]["v"] else {
        panic!("array result")
    };
    assert_eq!(array.dimensions(), [2]);
    let uqa_core::Value::Array(first) = &array.elements()[0] else {
        panic!("scalar array domain")
    };
    assert_eq!(first.lower_bounds(), [4]);
    assert_eq!(first.dimensions(), [2]);
    let expected = serde_json::json!([["[-2:-1]={\"[4:5]={7,8}\",\"{}\"}", "[-2:-1]", "2"]]);
    assert_eq!(
        crate::pg18_oracle::run_case(&engine, query)["results"][0]["rows"],
        expected
    );
    if provider != 0 {
        drop(engine);
        let engine = open(provider, &path);
        assert_eq!(
            crate::pg18_oracle::run_case(&engine, query)["results"][0]["rows"],
            expected
        );
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn host_array_binds_every_identity_before_input_callbacks(#[case] provider: usize) {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken, Value,
    };
    use uqa_sql::{ColumnType, SQLParam};
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("array-bind.db"));
    engine.sql("CREATE SEQUENCE array_calls; SELECT nextval('array_calls'); CREATE DOMAIN counted AS integer CHECK(nextval('array_calls') > 0); CREATE TYPE gone AS ENUM ('1')", &[]).unwrap();
    let control = ProductionControl::uncontrolled();
    let missing = engine
        .create_typed_parameter_with_control("gone", Some("1"), &control)
        .unwrap()
        .into_uncontrolled()
        .unwrap();
    engine.sql("DROP TYPE gone", &[]).unwrap();
    let elements = [
        SQLParam::typed_scalar(Value::Int(1), ColumnType::Integer),
        missing,
    ];
    let error = engine
        .create_array_parameter_with_control("counted[]", &[2], &[1], &elements, &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert_eq!(
        engine
            .sql("SELECT currval('array_calls') AS n", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );
    let budget = MemoryBudget::new(0);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let error = engine
        .create_array_parameter_with_control("counted[]", &[1], &[1], &elements[..1], &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    assert_eq!(
        engine
            .sql("SELECT currval('array_calls') AS n", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn domain_array_inputs_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("domain-array-inputs.db");
    let mut engine = open(provider, &path);
    let mut prefix: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/domain_array_input_oracle.expected.json"
    ))
    .unwrap();
    let mut suffix = prefix.clone();
    suffix["cases"] = prefix["cases"].as_array_mut().unwrap().split_off(16).into();
    crate::pg18_oracle::verify(&engine, &prefix.to_string());
    if provider != 0 {
        drop(engine);
        engine = open(provider, &path);
        let mut restored = prefix.clone();
        restored["cases"].as_array_mut().unwrap().drain(..2);
        crate::pg18_oracle::verify(&engine, &restored.to_string());
    }
    // Keep PREPARE and EXECUTE on the same session, including the domain change.
    // The transcript preserves earlier successful rows in the message that fails.
    crate::pg18_oracle::verify(&engine, &suffix.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn stored_domain_array_inputs_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("stored-domain-array-inputs.db");
    let mut engine = open(provider, &path);
    let mut prefix: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/domain_array_schema_oracle.expected.json"
    ))
    .unwrap();
    let mut restored = prefix.clone();
    restored["cases"] = prefix["cases"].as_array_mut().unwrap().split_off(32).into();
    crate::pg18_oracle::verify(&engine, &prefix.to_string());
    if provider != 0 {
        drop(engine);
        engine = open(provider, &path);
        let mut failures = prefix.clone();
        failures["cases"].as_array_mut().unwrap().retain(|case| {
            matches!(
                case["id"].as_str().unwrap(),
                "scalar_domain_default_assignment" | "null_domain_default_assignment"
            )
        });
        assert_eq!(failures["cases"].as_array().unwrap().len(), 2);
        // The loaded defaults must still perform their outer scalar constraints.
        crate::pg18_oracle::verify(&engine, &failures.to_string());
    }
    // nextval observes durable state without masking extra input checks at open.
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}
