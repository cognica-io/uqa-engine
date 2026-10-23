//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected function identities and consumer demand against independent `PostgreSQL` observations.

use uqa_engine::Engine;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn window_record_scope_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("window-record-scope.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/window_record_scope_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| !matches!(case["id"].as_str().unwrap(), "version" | "create_view"));
    assert_eq!(restored["cases"].as_array().unwrap().len(), 18);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn row_star_expansion_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("row-star-expansion.db");
    let engine = open(provider, &path);
    let transcript = include_str!("../../../../tests/parity/pg18/row_star_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| !matches!(case["id"].as_str().unwrap(), "version" | "create_view"));
    assert_eq!(restored["cases"].as_array().unwrap().len(), 15);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn scalar_predicate_capture_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("scalar-predicate-capture.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/scalar_predicate_capture_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"].as_array_mut().unwrap().retain(|case| {
        !matches!(
            case["id"].as_str().unwrap(),
            "version" | "distinct_capture_sequence" | "create_view"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 61);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn ordered_field_capture_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ordered-field-capture.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/ordered_field_capture_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["id"] != "version");
    assert_eq!(restored["cases"].as_array().unwrap().len(), 28);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn shadowed_column_diagnostics_match_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("shadowed-column.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/shadowed_column_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| !matches!(case["id"].as_str().unwrap(), "version"));
    assert_eq!(restored["cases"].as_array().unwrap().len(), 13);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn correlated_input_capture_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("correlated-capture.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/correlated_capture_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| !matches!(case["id"].as_str().unwrap(), "version" | "create_view"));
    assert_eq!(restored["cases"].as_array().unwrap().len(), 24);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn derived_input_capture_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("derived-capture.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/derived_capture_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| !matches!(case["id"].as_str().unwrap(), "version" | "create_view"));
    assert_eq!(restored["cases"].as_array().unwrap().len(), 15);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn derived_query_pullup_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("derived-pullup.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/derived_pullup_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"].as_array_mut().unwrap().retain(|case| {
        !matches!(
            case["id"].as_str().unwrap(),
            "version" | "sequence" | "stored_view" | "nullable_sequence"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 21);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[test]
fn explaining_derived_filters_keeps_output_effects_at_execution() {
    let engine = Engine::new();
    engine.sql("CREATE SEQUENCE explain_effect", &[]).unwrap();
    engine.sql("CREATE VIEW explain_derived AS SELECT x AS k,nextval('explain_effect') AS z FROM (VALUES(0),(2)) v(x)", &[]).unwrap();
    for sql in [
        "SELECT * FROM (SELECT x AS k,nextval('explain_effect') AS z FROM (VALUES(0),(2)) v(x)) s WHERE k>0",
        "SELECT * FROM explain_derived s(j) WHERE j>0",
        "WITH q AS NOT MATERIALIZED (SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x)) SELECT * FROM q WHERE k>0",
    ] {
        engine.sql("SELECT setval('explain_effect',1,false)", &[]).unwrap();
        engine.sql(&format!("EXPLAIN (FORMAT JSON) {sql}"), &[]).unwrap();
        assert_eq!(engine.sql("SELECT is_called FROM explain_effect", &[]).unwrap().rows[0]["is_called"], uqa_core::Value::Bool(false));
        let result = engine.sql(&format!("EXPLAIN (ANALYZE, FORMAT JSON) {sql}"), &[]).unwrap();
        let uqa_core::Value::Str(payload) = &result.rows[0]["plan"] else {
            panic!("JSON plan");
        };
        let plan: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert_eq!(plan["Actual Rows"], 1, "{sql}: {plan}");
        let expected = if sql.starts_with("WITH") { 1 } else { 2 };
        assert_eq!(engine.sql("SELECT nextval('explain_effect') AS n", &[]).unwrap().rows[0]["n"], uqa_core::Value::Int(expected));
    }
}

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
fn function_star_identity_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("function-star.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/function_star_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"].as_array_mut().unwrap().retain(|case| {
        !matches!(
            case["id"].as_str().unwrap(),
            "version" | "schema" | "routine" | "one_argument_routine"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 24);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn union_output_demand_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("union-demand.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/union_output_demand_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    engine
        .sql("SELECT setval('union_demand_probe.effects',1,false)", &[])
        .unwrap();
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"].as_array_mut().unwrap().retain(|case| {
        !matches!(
            case["id"].as_str().unwrap(),
            "version"
                | "schema"
                | "sequence"
                | "effect_schema"
                | "effect_stable_function"
                | "effect_volatile_function"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 50);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn join_output_demand_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("join-demand.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/join_output_demand_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn original_output_demand_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(
        provider,
        &directory.path().join("original-output-demand.db"),
    );
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/projection_output_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn derived_predicate_order_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("derived-predicates.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/derived_predicate_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "view_output"
                | "view_alias_tail"
                | "stored_read"
                | "stored_lateral_read"
                | "volatile_view_reset"
                | "volatile_view_read"
                | "volatile_view_effects"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 7);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn cte_definition_scopes_match_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cte-definition-scopes.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/cte_definition_scope_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "view_read"
                | "routine_read"
                | "catalog_same_name"
                | "different_types"
                | "column_aliases"
                | "quoted_names"
                | "definition_chain"
                | "repeated_not_materialized"
                | "recursive_self"
                | "recursive_forward"
                | "unused_enclosing_output"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 11);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}
