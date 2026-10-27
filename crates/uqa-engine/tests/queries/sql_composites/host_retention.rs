//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Host input keeps physical tuples through catalog changes and native parameter consumers.

use uqa_core::{memory::ProductionControl, Value};
use uqa_engine::Engine;
use uqa_sql::{ColumnType, SQLParam};

fn text(engine: &Engine, sql: &str, parameter: &SQLParam) -> String {
    let result = engine.sql(sql, std::slice::from_ref(parameter)).unwrap();
    let Some(Value::Str(value)) = result.value_at(0, 0) else {
        panic!("text result: {result:?}")
    };
    value.clone()
}

fn oracle(name: &str) -> Vec<String> {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("host_retention.pg18.json")).unwrap();
    reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["case"] == name)
        .unwrap()["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect()
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn partial_writes_rebuild_physical_composites_and_arrays(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("physical-writes.db");
    let engine = super::addition::open(provider, &path);
    engine.sql("CREATE TYPE assignment_leaf AS (a integer,b text); CREATE TYPE assignment_container AS (leaf assignment_leaf,leaves assignment_leaf[],nums integer[]); CREATE TABLE physical_writes(v assignment_container,a integer[]); INSERT INTO physical_writes(v) VALUES ((ROW('(7,x)'::assignment_leaf,ARRAY['(8,y)'::assignment_leaf],ARRAY[1,2])::assignment_container)::text::assignment_container); UPDATE physical_writes SET a=(v).nums", &[]).unwrap();
    let cases = [
        (
            "UPDATE physical_writes SET v.leaf.b='changed',v.leaves[1].a=80,v.nums[2]=20",
            "SELECT v::text,a::text FROM physical_writes",
            "physical_partial_writes",
        ),
        (
            "UPDATE physical_writes SET a[2]=99",
            "SELECT a::text FROM physical_writes",
            "physical_array_element_write",
        ),
        (
            "UPDATE physical_writes SET a[1:2]=(v).nums; ALTER TYPE assignment_leaf RENAME ATTRIBUTE a TO renamed CASCADE; UPDATE physical_writes SET v.leaf.renamed=70",
            "SELECT v::text,a::text FROM physical_writes",
            "physical_array_slice_and_renamed_field",
        ),
    ];
    for (mutation, query, name) in cases {
        engine.sql(mutation, &[]).unwrap();
        let result = engine.sql(query, &[]).unwrap();
        for (index, expected) in oracle(name).into_iter().enumerate() {
            assert_eq!(
                result.value_at(0, index),
                Some(&Value::Str(expected)),
                "{name}"
            );
        }
    }
    if provider != 0 {
        drop(engine);
        let engine = super::addition::open(provider, &path);
        let result = engine
            .sql("SELECT v::text,a::text FROM physical_writes", &[])
            .unwrap();
        for (index, expected) in oracle("physical_array_slice_and_renamed_field")
            .into_iter()
            .enumerate()
        {
            assert_eq!(result.value_at(0, index), Some(&Value::Str(expected)));
        }
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn native_consumers_observe_physical_inputs_without_reading_unselected_fields(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::addition::open(provider, &directory.path().join("physical-consumers.db"));
    engine
        .sql("CREATE TYPE native_json_row AS (a integer,b text)", &[])
        .unwrap();
    let queries = [
        "SELECT to_json('(7,x)'::native_json_row)::text",
        "SELECT json_build_array('(7,x)'::native_json_row)::text",
        "SELECT json_build_object('row','(7,x)'::native_json_row)::text",
        "SELECT json_agg(v)::text FROM (VALUES ('(7,x)'::native_json_row)) AS t(v)",
        "SELECT json_object_agg('row',v)::text FROM (VALUES ('(7,x)'::native_json_row)) AS t(v)",
    ];
    for (query, expected) in queries.into_iter().zip(oracle("native_json_output")) {
        assert_eq!(
            engine.sql(query, &[]).unwrap().value_at(0, 0),
            Some(&Value::Str(expected)),
            "{query}"
        );
    }
    engine.sql("CREATE DOMAIN native_json_real AS real; CREATE TYPE native_json_float_row AS (single real,domain_single native_json_real,wide double precision)", &[]).unwrap();
    for (query, expected) in [
        "SELECT to_json('(0.1,0.1,0.1)'::native_json_float_row)::text",
        "SELECT to_json(ROW(0.1::real,0.1::native_json_real,0.1::double precision))::text",
        "SELECT to_json(ARRAY[0.1::native_json_real])::text",
    ]
    .into_iter()
    .zip(oracle("native_json_float_width"))
    {
        assert_eq!(
            engine.sql(query, &[]).unwrap().value_at(0, 0),
            Some(&Value::Str(expected)),
            "{query}"
        );
    }
    let errors = oracle("native_json_key_errors");
    for (index, query) in [
        "SELECT json_build_object('(7,x)'::native_json_row,1)",
        "SELECT json_object_agg(v,1) FROM (VALUES ('(7,x)'::native_json_row)) AS t(v)",
    ]
    .into_iter()
    .enumerate()
    {
        let error = engine.sql(query, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(errors[index * 2].as_str()));
        assert_eq!(error.to_string(), errors[index * 2 + 1]);
    }
    engine.sql("CREATE TYPE unread_fields AS (a integer,tail integer); CREATE VIEW unread_fields_value AS SELECT '(7,287454020)'::unread_fields AS v; ALTER TYPE unread_fields ALTER ATTRIBUTE tail TYPE text", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT (v).a::text FROM unread_fields_value", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Str(oracle("unread_field_headers")[0].clone()))
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn host_composite_parameters_keep_original_physical_slots(#[case] provider: usize) {
    for structured in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("retained-host.db");
        let engine = super::addition::open(provider, &path);
        engine
            .sql(
                "CREATE TYPE retained_input AS (a integer, b text, tail integer)",
                &[],
            )
            .unwrap();
        let control = ProductionControl::uncontrolled();
        let fields = [
            SQLParam::typed_scalar(Value::Int(7), ColumnType::Integer),
            SQLParam::typed_scalar(Value::Str("x".into()), ColumnType::Text),
            SQLParam::typed_scalar(Value::Int(9), ColumnType::Integer),
        ];
        let retained = if structured {
            engine.create_record_parameter_with_control("retained_input", &fields, &control)
        } else {
            engine.create_typed_parameter_with_control("retained_input", Some("(7,x,9)"), &control)
        }
        .unwrap();
        let expected = oracle("rename_drop_add");
        assert_eq!(text(&engine, "SELECT $1::text", &retained), expected[0]);
        engine
            .sql(
                "ALTER TYPE retained_input RENAME ATTRIBUTE a TO renamed",
                &[],
            )
            .unwrap();
        assert_eq!(
            text(&engine, "SELECT ($1).renamed::text", &retained),
            expected[1]
        );
        engine.sql("ALTER TYPE retained_input DROP ATTRIBUTE b; ALTER TYPE retained_input ADD ATTRIBUTE added text", &[]).unwrap();
        assert_eq!(text(&engine, "SELECT $1::text", &retained), expected[2]);
        assert_eq!(
            text(&engine, "SELECT ($1).tail::text", &retained),
            expected[3]
        );
        engine
            .sql("CREATE TABLE retained_saved(v retained_input)", &[])
            .unwrap();
        engine
            .sql(
                "INSERT INTO retained_saved VALUES ($1)",
                std::slice::from_ref(&retained),
            )
            .unwrap();
        if provider != 0 {
            drop(engine);
            let engine = super::addition::open(provider, &path);
            let result = engine
                .sql("SELECT v::text FROM retained_saved", &[])
                .unwrap();
            assert_eq!(
                result.value_at(0, 0),
                Some(&Value::Str(expected[2].clone()))
            );
        }
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn host_composite_widening_preserves_lazy_reads_and_rollback(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::addition::open(provider, &directory.path().join("retained-width.db"));
    engine
        .sql(
            "CREATE TYPE retained_bits AS (a integer, b integer, c integer)",
            &[],
        )
        .unwrap();
    let control = ProductionControl::uncontrolled();
    let retained = engine
        .create_typed_parameter_with_control(
            "retained_bits",
            Some("(16909060,84281096,287454020)"),
            &control,
        )
        .unwrap();
    engine
        .sql(
            "BEGIN; ALTER TYPE retained_bits ALTER ATTRIBUTE a TYPE bigint; SAVEPOINT before_read",
            &[],
        )
        .unwrap();
    let expected = oracle("widen_in_bounds_and_null");
    for (sql, value) in [
        ("SELECT ($1).a::text", &expected[0]),
        ("SELECT ($1).b::text", &expected[1]),
        ("SELECT (($1).c IS NULL)::text", &expected[2]),
    ] {
        assert_eq!(text(&engine, sql, &retained), *value);
    }
    let error = engine
        .sql("SELECT ($1).c::text", std::slice::from_ref(&retained))
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX001"));
    assert_eq!(
        engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
        Some("25P02")
    );
    engine.sql("ROLLBACK TO before_read", &[]).unwrap();
    assert_eq!(text(&engine, "SELECT ($1).a::text", &retained), expected[0]);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        text(&engine, "SELECT $1::text", &retained),
        "(16909060,84281096,287454020)"
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn host_nested_composites_and_anonymous_array_inputs_keep_identity(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::addition::open(provider, &directory.path().join("retained-nested.db"));
    engine.sql("CREATE TYPE retained_child AS (a integer, b text); CREATE TYPE retained_parent AS (child retained_child, children retained_child[])", &[]).unwrap();
    let control = ProductionControl::uncontrolled();
    let child = engine
        .create_typed_parameter_with_control("retained_child", Some("(7,x)"), &control)
        .unwrap()
        .into_uncontrolled()
        .unwrap();
    let second = engine
        .create_typed_parameter_with_control("retained_child", Some("(8,y)"), &control)
        .unwrap()
        .into_uncontrolled()
        .unwrap();
    let array = engine
        .create_array_parameter_with_control("retained_child[]", &[1], &[1], &[second], &control)
        .unwrap()
        .into_uncontrolled()
        .unwrap();
    let input = [child, array];
    let parent = engine
        .create_record_parameter_with_control("retained_parent", &input, &control)
        .unwrap();
    let anonymous = engine
        .create_record_parameter_with_control("record", &input, &control)
        .unwrap();
    drop(input);
    engine
        .sql(
            "ALTER TYPE retained_child RENAME ATTRIBUTE a TO renamed",
            &[],
        )
        .unwrap();
    let expected = oracle("nested_composite_and_array");
    assert_eq!(
        text(&engine, "SELECT (($1).child).renamed::text", &parent),
        expected[0]
    );
    assert_eq!(
        text(
            &engine,
            "SELECT ((($1).children)[1]).renamed::text",
            &parent
        ),
        expected[1]
    );
    let expected = oracle("anonymous_composite_and_array");
    assert_eq!(text(&engine, "SELECT $1::text", &anonymous), expected[0]);
    for (sql, fields) in [
        "SELECT (($1).f1).renamed::text",
        "SELECT ((($1).f2)[1]).renamed::text",
    ]
    .into_iter()
    .zip(expected[1..].chunks_exact(2))
    {
        let error = engine
            .sql(sql, std::slice::from_ref(&*anonymous))
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some(fields[0].as_str()));
        assert_eq!(error.to_string(), fields[1]);
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn host_composite_domains_keep_values_before_catalog_mutating_checks(#[case] provider: usize) {
    for structured in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let engine = super::addition::open(provider, &directory.path().join("domain-input.db"));
        engine
            .sql(
                "CREATE SEQUENCE retained_callback_calls; \
            CREATE TYPE retained_callback_pair AS (a integer,b text); \
            CREATE FUNCTION retained_callback_rename(v retained_callback_pair) RETURNS boolean \
            LANGUAGE plpgsql AS $$BEGIN PERFORM nextval('retained_callback_calls'); \
            ALTER TYPE retained_callback_pair RENAME ATTRIBUTE a TO renamed; RETURN true; END$$; \
            CREATE DOMAIN retained_callback_domain AS retained_callback_pair \
            CHECK(retained_callback_rename(VALUE))",
                &[],
            )
            .unwrap();
        let control = ProductionControl::uncontrolled();
        let value = if structured {
            let fields = [
                SQLParam::typed_scalar(Value::Int(7), ColumnType::Integer),
                SQLParam::typed_scalar(Value::Str("x".into()), ColumnType::Text),
            ];
            engine.create_record_parameter_with_control(
                "retained_callback_domain",
                &fields,
                &control,
            )
        } else {
            engine.create_typed_parameter_with_control(
                "retained_callback_domain",
                Some("(7,x)"),
                &control,
            )
        }
        .unwrap();
        let expected = oracle("domain_input_catalog_change");
        assert_eq!(text(&engine, "SELECT $1::text", &value), expected[0]);
        assert_eq!(text(&engine, "SELECT ($1).renamed::text", &value), "7");
        assert_eq!(
            engine
                .sql("SELECT currval('retained_callback_calls')::text", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Str(expected[1].clone()))
        );
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn host_composite_field_checks_keep_the_original_input_descriptor(#[case] provider: usize) {
    for structured in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let engine = super::addition::open(provider, &directory.path().join("field-input.db"));
        engine.sql("CREATE SEQUENCE retained_field_calls; \
            CREATE DOMAIN retained_field_int AS integer; \
            CREATE TYPE retained_field_pair AS (a retained_field_int,b text); \
            CREATE FUNCTION retained_field_rename(v integer) RETURNS boolean \
            LANGUAGE plpgsql AS $$BEGIN PERFORM nextval('retained_field_calls'); \
            ALTER TYPE retained_field_pair RENAME ATTRIBUTE a TO renamed; RETURN true; END$$; \
            ALTER DOMAIN retained_field_int ADD CONSTRAINT mutate CHECK(retained_field_rename(VALUE))", &[]).unwrap();
        let control = ProductionControl::uncontrolled();
        let value = if structured {
            let fields = [
                SQLParam::typed_scalar(Value::Int(7), ColumnType::Integer),
                SQLParam::typed_scalar(Value::Str("x".into()), ColumnType::Text),
            ];
            engine.create_record_parameter_with_control("retained_field_pair", &fields, &control)
        } else {
            engine.create_typed_parameter_with_control(
                "retained_field_pair",
                Some("(7,x)"),
                &control,
            )
        }
        .unwrap();
        let expected = oracle("field_input_catalog_change");
        assert_eq!(text(&engine, "SELECT $1::text", &value), expected[0]);
        assert_eq!(text(&engine, "SELECT ($1).renamed::text", &value), "7");
        assert_eq!(
            engine
                .sql("SELECT currval('retained_field_calls')::text", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Str(expected[1].clone()))
        );
    }
}
