//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public call binding preserves independently captured native range distances.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ColumnType;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn wide_date_values_and_distance_calls_survive_provider_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wide-dates.db");
    let engine = super::range_input::open(provider, &path);
    engine
        .sql(
            "CREATE TABLE wide_dates (value date PRIMARY KEY, endpoints date[], span daterange)",
            &[],
        )
        .unwrap();
    engine.sql("INSERT INTO wide_dates VALUES (DATE '5874897-12-31', ARRAY[DATE '5874897-12-31', DATE '2000-01-01'], '[5874897-12-31,)'::daterange)", &[]).unwrap();
    engine.sql("CREATE FUNCTION stored_distance(v date DEFAULT DATE '5874897-12-31') RETURNS double precision LANGUAGE SQL RETURN daterange_subdiff(v, DATE '4713-01-01 BC')", &[]).unwrap();
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../uqa-sql/src/expr/range/subtype_distance/tests/postgresql.json"
    ))
    .unwrap();
    let expected = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| {
            case["function"] == "daterange_subdiff" && case["arguments"][0] == "5874897-12-31"
        })
        .unwrap()["expected"]["hex"]
        .as_str()
        .unwrap();
    let expected = u64::from_str_radix(expected, 16).unwrap();
    let verify = |engine: &Engine| {
        let result = engine.sql("SELECT value::text AS value, endpoints[1]::text AS member, span::text AS span, stored_distance(value) AS distance, stored_distance() AS default_distance FROM wide_dates WHERE value = DATE '5874897-12-31'", &[]).unwrap();
        assert_eq!(result.rows.len(), 1);
        for name in ["value", "member"] {
            assert_eq!(result.rows[0][name], Value::Str("5874897-12-31".into()));
        }
        assert_eq!(
            result.rows[0]["span"],
            Value::Str("[5874897-12-31,)".into())
        );
        for name in ["distance", "default_distance"] {
            let Value::Float(value) = result.rows[0][name] else {
                panic!("float8 distance")
            };
            assert_eq!(value.to_bits(), expected);
        }
    };
    verify(&engine);
    if provider != 0 {
        drop(engine);
        verify(&super::range_input::open(provider, &path));
    }
}

#[test]
fn date_range_calls_preserve_postgresql_finite_bounds_and_empty_order() {
    let engine = Engine::new();
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../uqa-sql/src/expr/range/production/tests/date_bounds.json"
    ))
    .unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let result = engine.sql(sql, &[]);
        if let Some(expected) = case["error"].as_object() {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), expected["sqlstate"].as_str(), "{sql}");
            assert_eq!(error.to_string(), expected["message"].as_str().unwrap());
            assert_eq!(error.detail(), expected["detail"].as_str());
            assert_eq!(error.hint(), expected["hint"].as_str());
        } else {
            let result = result.unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert_eq!(
                result.rows[0]["value"],
                Value::Str(case["expected"].as_str().unwrap().into())
            );
        }
    }
}

#[test]
fn wide_date_inputs_and_output_match_postgresql_before_distance_evaluation() {
    let engine = Engine::new();
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../uqa-core/src/types/temporal/calendar/tests/postgresql.json"
    ))
    .unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let sql = format!("SELECT d::text AS output, d - DATE '1970-01-01' AS days FROM (SELECT DATE '{input}' AS d) AS input");
        let result = engine.sql(&sql, &[]);
        let expected = &case["expected"];
        if let Some(expected) = expected.get("error") {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), expected["sqlstate"].as_str(), "{sql}");
            assert_eq!(
                error.to_string(),
                expected["message"].as_str().unwrap(),
                "{sql}"
            );
            assert_eq!(error.detail(), expected["detail"].as_str());
            assert_eq!(error.hint(), expected["hint"].as_str());
        } else {
            let result = result.unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert_eq!(
                result.rows[0]["output"],
                Value::Str(expected["output"].as_str().unwrap().into())
            );
            assert_eq!(
                result.rows[0]["days"],
                Value::Int(expected["days"].as_str().unwrap().parse().unwrap())
            );
        }
    }
}

#[test]
fn range_subtype_distance_calls_match_postgresql_values_types_and_errors() {
    let engine = Engine::new();
    engine.sql("SET TimeZone='UTC'", &[]).unwrap();
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../uqa-sql/src/expr/range/subtype_distance/tests/postgresql.json"
    ))
    .unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let sql = format!(
            "SELECT {} AS distance",
            case["expression"].as_str().unwrap()
        );
        let result = engine.sql(&sql, &[]);
        let expected = &case["expected"];
        if let Some(expected) = expected.get("error") {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), expected["sqlstate"].as_str(), "{sql}");
            assert_eq!(
                error.to_string(),
                expected["message"].as_str().unwrap(),
                "{sql}"
            );
            assert_eq!(error.detail(), expected["detail"].as_str());
            assert_eq!(error.hint(), expected["hint"].as_str());
        } else {
            let result = result.unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert_eq!(
                result.column_types,
                [Some(ColumnType::DoublePrecision)],
                "{sql}"
            );
            let actual = &result.rows[0]["distance"];
            if let Some(bits) = expected["hex"].as_str() {
                let Value::Float(value) = actual else {
                    panic!("{sql}: {actual:?}")
                };
                assert_eq!(
                    value.to_bits(),
                    u64::from_str_radix(bits, 16).unwrap(),
                    "{sql}"
                );
            } else {
                assert_eq!(*actual, Value::Null, "{sql}");
            }
        }
    }
}

#[test]
fn range_subtype_distance_binding_keeps_schema_precedence_and_explicit_signatures() {
    let engine = Engine::new();
    engine.sql("CREATE SCHEMA distance_app", &[]).unwrap();
    engine.sql("CREATE FUNCTION distance_app.int4range_subdiff(integer,integer) RETURNS integer LANGUAGE SQL RETURN 17", &[]).unwrap();
    engine
        .sql("SET search_path=distance_app,pg_catalog", &[])
        .unwrap();
    let result = engine
        .sql(
            "SELECT int4range_subdiff(1,3) AS custom, pg_catalog.int4range_subdiff(1,3) AS native",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["custom"], Value::Int(17));
    assert_eq!(result.rows[0]["native"], Value::Float(-2.0));
    assert_eq!(
        result.column_types,
        [Some(ColumnType::Integer), Some(ColumnType::DoublePrecision)]
    );
    for sql in [
        "SELECT pg_catalog.int4range_subdiff(1)",
        "SELECT pg_catalog.int4range_subdiff('1'::text,'2'::text)",
        "SELECT pg_catalog.int4range_subdiff(1::bigint,2::bigint)",
    ] {
        assert_eq!(
            engine.sql(sql, &[]).unwrap_err().sqlstate(),
            Some("42883"),
            "{sql}"
        );
    }
}
