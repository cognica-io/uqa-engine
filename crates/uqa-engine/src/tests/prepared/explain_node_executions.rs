//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use uqa_core::Value;

fn explanation(engine: &Engine, query: &str, timing: bool) -> serde_json::Value {
    let sql = format!("EXPLAIN (ANALYZE, TIMING {timing}, BUFFERS FALSE, FORMAT JSON) {query}");
    let result = engine
        .sql(&sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let Some(Value::Json(text)) = result.value_at(0, 0) else {
        panic!("JSON explanation")
    };
    serde_json::from_str::<serde_json::Value>(text).unwrap()[0].clone()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_source_measurements_survive_prepared_and_aggregate_paths(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("node-executions.db"));
    engine.sql("CREATE TABLE measured_input(v integer); INSERT INTO measured_input VALUES (0),(1),(2),(3),(4),(5),(6),(7); PREPARE measured_selection(integer) AS SELECT v FROM measured_input WHERE v > $1", &[]).unwrap();
    for timing in [false, true] {
        for (query, output_rows, scan_rows) in [
            ("SELECT v FROM measured_input", 8, 8),
            (
                "SELECT v + 1 FROM measured_input WHERE v > 3 ORDER BY v",
                4,
                4,
            ),
            ("SELECT sum(v) FROM measured_input", 1, 8),
            ("EXECUTE measured_selection(3)", 4, 4),
        ] {
            let document = explanation(&engine, query, timing);
            assert_eq!(
                document["Plan"]["Actual Rows"], output_rows,
                "{query}: {document}"
            );
            let nodes = document["Node Executions"]
                .as_array()
                .expect("native execution instances");
            let scans = nodes
                .iter()
                .filter(|node| node["Node Type"] == "Relation Scan")
                .collect::<Vec<_>>();
            assert_eq!(scans.len(), 1, "{query}: {document}");
            assert_eq!(scans[0]["Actual Rows"], scan_rows, "{query}: {document}");
            assert_eq!(scans[0]["Actual Loops"], 1, "{query}: {document}");
            assert!(
                nodes
                    .iter()
                    .any(|node| node["Node Type"] == "Query Pipeline"
                        && node["Actual Rows"] == output_rows),
                "{query}: {document}"
            );
            for node in nodes {
                assert_eq!(node.get("Actual Total Time").is_some(), timing);
                if timing {
                    assert!(
                        node["Actual Startup Time"].as_f64().unwrap()
                            <= node["Actual Total Time"].as_f64().unwrap()
                    );
                }
            }
        }
    }
    engine.close().unwrap();
}

fn relation_rows(document: &serde_json::Value) -> Vec<f64> {
    document["Node Executions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["Node Type"] == "Relation Scan")
        .map(|node| {
            assert_eq!(node["Actual Loops"], 1, "{document}");
            node["Actual Rows"].as_f64().unwrap()
        })
        .collect()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn retained_cursor_instances_measure_only_the_fetching_invocation(#[case] provider: Provider) {
    for scroll in ["SCROLL", "NO SCROLL"] {
        let directory = tempfile::tempdir().unwrap();
        let engine = Arc::new(provider.open(&directory.path().join("retained-executions.db")));
        engine.sql("CREATE TABLE retained_input(v integer); INSERT INTO retained_input VALUES (0),(1),(2),(3),(4),(5),(6),(7)", &[]).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let weak = Arc::downgrade(&engine);
        engine
            .register_scalar_function("fetch_one", move |_args: &[Value]| {
                let result = weak
                    .upgrade()
                    .unwrap()
                    .sql("FETCH 1 FROM measured_cursor", &[])?;
                assert_eq!(result.rows.len(), 1);
                counted.fetch_add(1, Ordering::Relaxed);
                Ok(Value::Int(1))
            })
            .unwrap();
        engine.sql(&format!("BEGIN; DECLARE measured_cursor {scroll} CURSOR FOR SELECT v FROM retained_input"), &[]).unwrap();
        // Construct the retained source before any collector exists.
        assert_eq!(
            engine
                .sql("FETCH 1 FROM measured_cursor", &[])
                .unwrap()
                .rows
                .len(),
            1
        );
        let mut documents = Vec::new();
        for timing in [false, true] {
            let document = explanation(&engine, "SELECT fetch_one()", timing);
            assert_eq!(relation_rows(&document), vec![1.0], "{scroll}: {document}");
            for node in document["Node Executions"].as_array().unwrap() {
                assert_eq!(node.get("Actual Total Time").is_some(), timing);
            }
            documents.push(document);
        }
        let nested = Arc::new(Mutex::new(None));
        let saved = Arc::clone(&nested);
        let weak = Arc::downgrade(&engine);
        engine
            .register_scalar_function("measure_fetch", move |_args: &[Value]| {
                let document = explanation(&weak.upgrade().unwrap(), "SELECT fetch_one()", false);
                *saved.lock().unwrap() = Some(document);
                Ok(Value::Int(1))
            })
            .unwrap();
        let outer = explanation(&engine, "SELECT measure_fetch()", true);
        assert_eq!(relation_rows(&outer), vec![1.0], "{scroll}: {outer}");
        let inner = nested.lock().unwrap().take().unwrap();
        assert_eq!(relation_rows(&inner), vec![1.0], "{scroll}: {inner}");
        assert!(inner["Node Executions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|node| node.get("Actual Total Time").is_none()));
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        assert_eq!(
            engine
                .sql("FETCH 1 FROM measured_cursor", &[])
                .unwrap()
                .rows
                .len(),
            1
        );
        for document in &documents {
            assert_eq!(relation_rows(document), vec![1.0]);
        }
        engine.sql("CLOSE measured_cursor; ROLLBACK", &[]).unwrap();
        engine.close().unwrap();
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn lateral_instances_retain_their_native_row_streams(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("lateral-executions.db"));
    for (query, kind, expected) in [
        ("SELECT g FROM (VALUES (2),(3)) v(n) CROSS JOIN LATERAL generate_series(1,n) g", "Lateral Function Scan", vec![2.0, 3.0]),
        ("SELECT * FROM (VALUES (2),(3)) v(n) CROSS JOIN LATERAL ROWS FROM (generate_series(1,n), generate_series(1,n+1)) AS g(a,b)", "Lateral Function Group Scan", vec![3.0, 4.0]),
    ] {
        let document = explanation(&engine, query, false);
        let rows = document["Node Executions"].as_array().unwrap().iter()
            .filter(|node| node["Node Type"] == kind)
            .map(|node| {
                assert_eq!(node["Actual Loops"], 1, "{query}: {document}");
                node["Actual Rows"].as_f64().unwrap()
            }).collect::<Vec<_>>();
        assert_eq!(rows, expected, "{query}: {document}");
    }
    engine.close().unwrap();
}
