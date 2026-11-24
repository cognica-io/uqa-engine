//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use serde_json::Value as Json;
use uqa_core::Value;

const SCALAR: &str = "SELECT b.v FROM b WHERE b.id = (SELECT a.b_id FROM a WHERE a.id = 'a77')";
const MEMBERSHIP: &str =
    "SELECT b.v FROM b WHERE b.id IN (SELECT a.b_id FROM a WHERE a.id = 'a77')";
const JOIN: &str = "SELECT b.v FROM a JOIN b ON b.id = a.b_id WHERE a.id = 'a77'";
const CHAIN: &str =
    "SELECT b.v FROM s JOIN a ON a.id = s.a_id JOIN b ON b.id = a.b_id WHERE s.id = 's77'";

fn explain(engine: &Engine, query: &str) -> Json {
    let result = engine
        .sql(
            &format!("EXPLAIN (ANALYZE, TIMING FALSE, FORMAT JSON) {query}"),
            &[],
        )
        .unwrap_or_else(|err| panic!("{query}: {err}"));
    let Some(Value::Json(text)) = result.value_at(0, 0) else {
        panic!("missing JSON plan for {query}");
    };
    let document: Json = serde_json::from_str(text).unwrap();
    document[0].clone()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn selective_runtime_keys_bound_relation_access(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("runtime-keys.db"));
    engine
        .sql(
            "CREATE TABLE a (id text PRIMARY KEY, b_id text NOT NULL);
        CREATE TABLE b (id text PRIMARY KEY, v text NOT NULL);
        CREATE TABLE s (id text PRIMARY KEY, a_id text NOT NULL);
        INSERT INTO a SELECT 'a' || i, 'b' || i FROM generate_series(1,256) AS g(i);
        INSERT INTO b SELECT 'b' || i, 'v' || i FROM generate_series(1,256) AS g(i);
        INSERT INTO s SELECT 's' || i, 'a' || i FROM generate_series(1,256) AS g(i);
        ANALYZE a; ANALYZE b; ANALYZE s",
            &[],
        )
        .unwrap();

    for (query, relation_count) in [(SCALAR, 2), (MEMBERSHIP, 2), (JOIN, 2), (CHAIN, 3)] {
        let result = engine.sql(query, &[]).unwrap();
        assert_eq!(result.rows.len(), 1, "{query}");
        assert_eq!(
            result.value_at(0, 0),
            Some(&Value::Str("v77".into())),
            "{query}"
        );
        let document = explain(&engine, query);
        let relations = document["Node Estimates"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["Node Type"] == "Relation")
            .collect::<Vec<_>>();
        assert_eq!(relations.len(), relation_count, "{query}: {relations:?}");
        for node in relations {
            let rows = node["Plan Rows"].as_f64().unwrap();
            let cost = node["Total Cost"].as_f64().unwrap();
            assert!(rows <= 1.0 && cost < 64.0, "{query}: {node}");
        }
        for scan in document["Node Executions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["Node Type"] == "Relation Scan")
        {
            assert!(
                scan["Actual Rows"].as_f64().unwrap() <= 1.0,
                "{query}: {scan}"
            );
        }
    }
    engine.close().unwrap();
}
