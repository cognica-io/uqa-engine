//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use uqa_core::Value;

fn json(engine: &Engine, sql: &str) -> serde_json::Value {
    let result = engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let Value::Json(text) = result.value_at(0, 0).unwrap() else {
        panic!("EXPLAIN JSON carrier");
    };
    serde_json::from_str(text).unwrap()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn costs_visibility_and_fixed_sql_widths_match_postgresql(#[case] provider: Provider) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_costs.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-costs.db"));
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let result = engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let text = result
            .rows
            .iter()
            .map(|row| match &row["QUERY PLAN"] {
                Value::Str(text) | Value::Json(text) => text.as_str(),
                other => panic!("unexpected EXPLAIN carrier {other:?}"),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let enabled = case["costs"].as_bool().unwrap();
        match case["format"].as_str().unwrap() {
            "JSON" => {
                let document: serde_json::Value = serde_json::from_str(&text).unwrap();
                let plan = &document[0]["Plan"];
                for key in ["Startup Cost", "Total Cost", "Plan Rows", "Plan Width"] {
                    assert_eq!(plan.get(key).is_some(), enabled, "{sql}: {key}");
                }
                if enabled {
                    assert_eq!(plan["Plan Width"], case["width"], "{sql}");
                    assert_eq!(plan["Plan Rows"].as_f64(), case["rows"].as_f64(), "{sql}");
                    let start = plan["Startup Cost"].as_f64().unwrap();
                    let total = plan["Total Cost"].as_f64().unwrap();
                    assert!(start.is_finite() && start >= 0.0 && total >= start);
                }
            }
            "TEXT" => {
                assert_eq!(text.contains("cost="), enabled, "{sql}");
                if enabled {
                    assert!(
                        text.contains(&format!(" rows={} width={}", case["rows"], case["width"])),
                        "{sql}: {text}"
                    );
                }
            }
            "XML" => {
                uqa_sql::xml::validate(&text, uqa_sql::xml::XmlOption::Document).unwrap();
                for key in ["Startup-Cost", "Total-Cost", "Plan-Rows", "Plan-Width"] {
                    assert_eq!(text.contains(&format!("<{key}>")), enabled, "{sql}");
                }
                if enabled {
                    assert!(
                        text.contains(&format!("<Plan-Width>{}</Plan-Width>", case["width"])),
                        "{sql}"
                    );
                }
            }
            "YAML" => {
                for key in ["Startup Cost", "Total Cost", "Plan Rows", "Plan Width"] {
                    assert_eq!(text.contains(key), enabled, "{sql}");
                }
                if enabled {
                    assert!(
                        text.contains(&format!("\"Plan Width\": {}", case["width"])),
                        "{sql}: {text}"
                    );
                }
            }
            _ => unreachable!(),
        }
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn estimates_use_native_statistics_without_invoking_the_query(#[case] provider: Provider) {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-cost-statistics.db"));
    engine.sql("CREATE TABLE cost_items (id integer); INSERT INTO cost_items SELECT generate_series(1,100)", &[]).unwrap();
    let scan = json(&engine, "EXPLAIN (FORMAT JSON) SELECT id FROM cost_items");
    let limited = json(
        &engine,
        "EXPLAIN (FORMAT JSON) SELECT id FROM cost_items LIMIT 4 OFFSET 2",
    );
    assert_eq!(scan[0]["Plan"]["Plan Rows"].as_f64(), Some(100.0));
    assert_eq!(limited[0]["Plan"]["Plan Rows"].as_f64(), Some(4.0));
    assert_eq!(scan[0]["Plan"]["Plan Width"], 4);
    assert!(
        limited[0]["Plan"]["Total Cost"].as_f64().unwrap()
            < scan[0]["Plan"]["Total Cost"].as_f64().unwrap()
    );
    let sorted = json(
        &engine,
        "EXPLAIN (FORMAT JSON) SELECT id FROM cost_items ORDER BY -id",
    );
    assert!(sorted[0]["Plan"]["Startup Cost"].as_f64().unwrap() > 0.0);
    engine
        .sql("PREPARE cost_input (smallint) AS SELECT $1, true", &[])
        .unwrap();
    assert_eq!(
        json(&engine, "EXPLAIN (FORMAT JSON) EXECUTE cost_input(4)")[0]["Plan"]["Plan Width"],
        3
    );
    assert_eq!(
        json(
            &engine,
            "EXPLAIN (GENERIC_PLAN, FORMAT JSON) SELECT $2::bigint"
        )[0]["Plan"]["Plan Width"],
        8
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    engine
        .register_scalar_function("cost_effect", move |_: &[Value]| {
            counted.fetch_add(1, Ordering::Relaxed);
            Ok(Value::Int(7))
        })
        .unwrap();
    for options in ["COSTS TRUE", "COSTS FALSE"] {
        json(
            &engine,
            &format!("EXPLAIN ({options}, FORMAT JSON) SELECT cost_effect()"),
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
    json(
        &engine,
        "EXPLAIN (ANALYZE, FORMAT JSON) SELECT cost_effect()",
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    engine.sql("BEGIN; EXPLAIN (FORMAT JSON) INSERT INTO cost_items VALUES (101) RETURNING id; ROLLBACK", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) FROM cost_items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(100))
    );
}
