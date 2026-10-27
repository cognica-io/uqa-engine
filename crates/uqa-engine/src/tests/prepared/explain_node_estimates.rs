//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;
use uqa_core::Value;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn child_estimates_keep_postgresql_cte_cardinalities_and_native_scopes(#[case] provider: Provider) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_node_estimates.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("node-estimates.db"));
    for case in reference["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let result = engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let Some(Value::Json(text)) = result.value_at(0, 0) else {
            panic!("JSON output")
        };
        let document: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            document[0]["Plan"]["Plan Rows"].as_f64(),
            case["rows"].as_f64(),
            "{sql}"
        );
        let nodes = document[0]["Node Estimates"].as_array().unwrap();
        let mut scans = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            assert_eq!(node["Node ID"].as_u64(), Some(index as u64));
            if index == 0 {
                assert!(node.get("Parent Node ID").is_none());
                assert_eq!(node["Plan Width"], document[0]["Plan"]["Plan Width"]);
            } else {
                assert!(node["Parent Node ID"].as_u64().unwrap() < index as u64);
            }
            let startup = node["Startup Cost"].as_f64().unwrap();
            let total = node["Total Cost"].as_f64().unwrap();
            assert!(
                startup.is_finite() && total.is_finite() && 0.0 <= startup && startup <= total,
                "{sql}: {node}"
            );
            if let Some(name) = node["Relation Name"]
                .as_str()
                .filter(|_| node["Node Type"] == "Relation")
            {
                scans.push((name, node["Plan Rows"].as_f64().unwrap()));
            }
            if node["Node Type"] == "Named Query" {
                assert!(
                    nodes
                        .iter()
                        .any(|child| child["Parent Node ID"] == node["Node ID"]),
                    "selected query was not captured: {sql}: {node}"
                );
            }
        }
        scans.sort_by(|left, right| left.0.cmp(right.0).then(left.1.total_cmp(&right.1)));
        let expected = case["cte_rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| {
                (
                    node["relation"].as_str().unwrap(),
                    node["rows"].as_f64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(scans, expected, "{sql}: {nodes:?}");
        let disabled = engine
            .sql(&sql.replace("COSTS TRUE", "COSTS FALSE"), &[])
            .unwrap();
        let Some(Value::Json(text)) = disabled.value_at(0, 0) else {
            panic!("JSON output")
        };
        let document: serde_json::Value = serde_json::from_str(text).unwrap();
        assert!(document[0].get("Node Estimates").is_none());
    }
    engine.close().unwrap();
}
