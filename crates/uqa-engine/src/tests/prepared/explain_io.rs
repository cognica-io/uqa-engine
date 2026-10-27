//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use serde_json::Value as Json;
use std::sync::{Arc, Mutex};
use uqa_core::Value;

const SPILL: &str = "SELECT n FROM generate_series(1,4096) AS input(n) ORDER BY n DESC";

fn document(engine: &Engine, sql: &str) -> Json {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let Some(Value::Json(text)) = result.value_at(0, 0) else {
        panic!("JSON EXPLAIN result");
    };
    serde_json::from_str::<Json>(text).unwrap()[0].clone()
}

fn explain(engine: &Engine, body: &str) -> Json {
    document(
        engine,
        &format!("EXPLAIN (ANALYZE, TIMING FALSE, SUMMARY FALSE, FORMAT JSON) {body}"),
    )
}

fn reads(plan: &Json) -> u64 {
    plan["Temp Read Blocks"].as_u64().unwrap()
}

fn node_buffers(document: &Json, buffers: bool, io_timing: bool) {
    let nodes = document["Node Executions"].as_array().unwrap();
    for node in nodes {
        for key in ["Temp Read Blocks", "Temp Written Blocks"] {
            assert_eq!(node.get(key).is_some(), buffers, "{document}");
            if buffers {
                assert!(
                    node[key].as_u64().unwrap() <= document["Plan"][key].as_u64().unwrap(),
                    "{document}"
                );
            }
        }
        for key in ["Temp I/O Read Time", "Temp I/O Write Time"] {
            assert_eq!(node.get(key).is_some(), buffers && io_timing, "{document}");
            if let Some(value) = node.get(key) {
                assert!(
                    value.as_f64().unwrap().is_finite() && value.as_f64().unwrap() >= 0.0,
                    "{document}"
                );
            }
        }
    }
    if buffers && reads(&document["Plan"]) > 0 {
        assert!(nodes.iter().any(|node| reads(node) > 0), "{document}");
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn planning_plan_and_serialization_io_visibility_match_postgresql(#[case] provider: Provider) {
    let reference: Json = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/io_timing.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-io-partition.db"));
    engine.sql("SET work_mem = '64kB'; CREATE FUNCTION change_io_timing(enabled text) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN PERFORM n FROM generate_series(1,4096) AS input(n) ORDER BY n DESC; PERFORM set_config('track_io_timing', enabled, false); RETURN 1; END $$", &[]).unwrap();
    engine
        .sql("PREPARE io_parameter(integer) AS SELECT $1", &[])
        .unwrap();
    for case in reference["partitioning"].as_array().unwrap() {
        engine
            .sql("DROP TABLE IF EXISTS io_destination", &[])
            .unwrap();
        engine.sql(case["setup"].as_str().unwrap(), &[]).unwrap();
        let doc = document(&engine, case["sql"].as_str().unwrap());
        for (expected, section) in [
            ("planning", "Planning"),
            ("plan", "Plan"),
            ("serialization", "Serialization"),
        ] {
            if case[expected].is_null() {
                assert!(doc.get(section).is_none(), "{case}: {doc}");
                continue;
            }
            let observed = doc
                .get(section)
                .unwrap_or_else(|| panic!("missing {section}: {case}: {doc}"));
            for (expected_field, field) in [
                ("read", "Temp Read Blocks"),
                ("written", "Temp Written Blocks"),
            ] {
                assert_eq!(
                    observed.get(field).map(|value| value.as_u64().unwrap() > 0),
                    case[expected][expected_field].as_bool(),
                    "{section}: {case}: {doc}"
                );
            }
            for field in ["Temp I/O Read Time", "Temp I/O Write Time"] {
                assert_eq!(
                    observed.get(field).is_some(),
                    case[expected]["io_time"].as_bool().unwrap(),
                    "{section}: {case}: {doc}"
                );
                if let Some(value) = observed.get(field) {
                    assert!(
                        value.as_f64().unwrap().is_finite() && value.as_f64().unwrap() >= 0.0,
                        "{case}: {doc}"
                    );
                }
            }
        }
        assert_eq!(
            doc.get("Planning Time").is_some(),
            case["planning_time"].as_bool().unwrap(),
            "{case}: {doc}"
        );
        assert_eq!(
            doc["Plan"].get("Actual Loops").and_then(Json::as_u64),
            case["loops"].as_u64(),
            "{case}: {doc}"
        );
    }
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn temporary_io_fields_and_live_clock_match_postgresql(#[case] provider: Provider) {
    let reference: Json = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/io_timing.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-io.db"));
    engine.sql("SET work_mem = '64kB'; CREATE FUNCTION change_io_timing(enabled text) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN PERFORM n FROM generate_series(1,4096) AS input(n) ORDER BY n DESC; PERFORM set_config('track_io_timing', enabled, false); RETURN 1; END $$", &[]).unwrap();
    for case in reference["explanations"].as_array().unwrap() {
        engine.sql(case["setup"].as_str().unwrap(), &[]).unwrap();
        let doc = document(&engine, case["sql"].as_str().unwrap());
        node_buffers(
            &doc,
            !case["read"].is_null(),
            case["io_time"].as_bool().unwrap(),
        );
        let plan = &doc["Plan"];
        for (expected, key) in [
            ("read", "Temp Read Blocks"),
            ("written", "Temp Written Blocks"),
        ] {
            let actual = plan.get(key).map(|value| value.as_u64().unwrap() > 0);
            assert_eq!(actual, case[expected].as_bool(), "{case}: {doc}");
        }
        assert_eq!(
            plan.get("Temp I/O Read Time").is_some(),
            case["io_time"].as_bool().unwrap(),
            "{case}: {doc}"
        );
        assert_eq!(
            plan.get("Actual Total Time").is_some(),
            case["operator_time"].as_bool().unwrap(),
            "{case}: {doc}"
        );
        for key in ["Temp I/O Read Time", "Temp I/O Write Time"] {
            if let Some(time) = plan.get(key) {
                let time = time.as_f64().unwrap();
                assert!(time.is_finite() && time >= 0.0, "{case}: {doc}");
                if case.get("function").is_some() {
                    assert_eq!(
                        time, 0.0,
                        "clock was disabled throughout physical work: {doc}"
                    );
                }
            }
        }
    }
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn every_format_obeys_buffers_without_using_operator_timing(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("io-formats.db"));
    engine
        .sql("SET work_mem = '64kB'; SET track_io_timing = on", &[])
        .unwrap();
    for format in ["TEXT", "JSON", "XML", "YAML"] {
        for buffers in [false, true] {
            let sql = format!("EXPLAIN (ANALYZE, BUFFERS {buffers}, TIMING FALSE, SUMMARY FALSE, FORMAT {format}) {SPILL}");
            let result = engine
                .sql(&sql, &[])
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            let output = (0..result.rows.len())
                .map(|row| match result.value_at(row, 0).unwrap() {
                    Value::Json(text) | Value::Str(text) => text.as_str(),
                    other => panic!("{other:?}"),
                })
                .collect::<Vec<_>>()
                .join("\n");
            let (blocks, timing) = match format {
                "TEXT" => ("Buffers: temp", "I/O Timings: temp"),
                "XML" => ("Temp-Read-Blocks", "Temp-I-O-Read-Time"),
                _ => ("Temp Read Blocks", "Temp I/O Read Time"),
            };
            assert_eq!(output.contains(blocks), buffers, "{sql}: {output}");
            assert_eq!(output.contains(timing), buffers, "{sql}: {output}");
        }
    }
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn nested_explanations_include_their_own_io_and_exclude_another_session(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = Arc::new(provider.open(&directory.path().join("io-nested.db")));
    let other = Arc::new(provider.open(&directory.path().join("io-independent.db")));
    for selected in [&engine, &other] {
        selected.sql("SET work_mem = '64kB'", &[]).unwrap();
    }
    let saved = Arc::new(Mutex::new(None));
    let output = Arc::clone(&saved);
    let weak = Arc::downgrade(&engine);
    engine
        .register_scalar_function("measure_spill", move |_args: &[Value]| {
            *output.lock().unwrap() = Some(explain(&weak.upgrade().unwrap(), SPILL));
            Ok(Value::Int(1))
        })
        .unwrap();
    let unrelated = Arc::clone(&other);
    engine
        .register_scalar_function("other_spill", move |_args: &[Value]| {
            assert!(reads(&explain(&unrelated, SPILL)["Plan"]) > 0);
            Ok(Value::Int(1))
        })
        .unwrap();
    let outer = explain(&engine, "SELECT measure_spill(), other_spill()");
    let inner = saved.lock().unwrap().take().unwrap();
    node_buffers(&outer, true, false);
    node_buffers(&inner, true, false);
    assert!(reads(&inner["Plan"]) > 0);
    assert_eq!(
        reads(&outer["Plan"]),
        reads(&inner["Plan"]),
        "{outer}: {inner}"
    );
    assert_eq!(
        outer["Plan"]["Temp Written Blocks"],
        inner["Plan"]["Temp Written Blocks"]
    );
    assert_eq!(reads(&explain(&engine, "SELECT 1")["Plan"]), 0);
    engine.close().unwrap();
    other.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn cursor_spill_reads_follow_the_fetching_invocation(#[case] provider: Provider) {
    for scroll in ["SCROLL", "NO SCROLL"] {
        let directory = tempfile::tempdir().unwrap();
        let engine = Arc::new(provider.open(&directory.path().join("io-cursor.db")));
        engine.sql(&format!("SET work_mem = '64kB'; BEGIN; DECLARE measured_io {scroll} CURSOR FOR {SPILL}; FETCH 1 FROM measured_io"), &[]).unwrap();
        let weak = Arc::downgrade(&engine);
        engine
            .register_scalar_function("fetch_io", move |_args: &[Value]| {
                let result = weak
                    .upgrade()
                    .unwrap()
                    .sql("FETCH 1000 FROM measured_io", &[])?;
                assert_eq!(result.rows.len(), 1000);
                Ok(Value::Int(1))
            })
            .unwrap();
        for _ in 0..2 {
            let measured = explain(&engine, "SELECT fetch_io()");
            node_buffers(&measured, true, false);
            assert!(reads(&measured["Plan"]) > 0, "{scroll}: {measured}");
            engine.sql("FETCH 1000 FROM measured_io", &[]).unwrap();
            assert_eq!(reads(&explain(&engine, "SELECT 1")["Plan"]), 0);
        }
        engine.sql("CLOSE measured_io; ROLLBACK", &[]).unwrap();
        engine.close().unwrap();
    }
}
