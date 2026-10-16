//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public catalog consumers and retained range fields survive actual provider reopen.

use uqa_core::Value;
use uqa_engine::Engine;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn native_range_catalog_defaults_match_postgresql_after_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("range-catalog.db");
    let engine = super::range_input::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../uqa-sql/src/expr/range/datum/fixtures.json"
    ))
    .unwrap();
    let cases = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| {
            case["id"].as_str().unwrap().ends_with("bounded")
                && !case["id"].as_str().unwrap().ends_with("unbounded")
                || case["id"].as_str().unwrap().ends_with("nullable")
        })
        .collect::<Vec<_>>();
    assert_eq!(cases.len(), 24);
    let columns = cases
        .iter()
        .enumerate()
        .map(|(index, case)| {
            let ty = case["type"].as_str().unwrap();
            let input = case["input"].as_str().unwrap().replace('\'', "''");
            format!("c{index} {ty} DEFAULT '{input}'::{ty}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    engine
        .sql(
            &format!("CREATE TABLE native_range_defaults ({columns})"),
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO native_range_defaults DEFAULT VALUES", &[])
        .unwrap();
    verify_defaults(&engine, &cases);
    if provider != 0 {
        drop(engine);
        verify_defaults(&super::range_input::open(provider, &path), &cases);
    }
}

fn verify_defaults(engine: &Engine, cases: &[&serde_json::Value]) {
    let catalog = engine.sql("SELECT d.adbin::text AS node, pg_get_expr(d.adbin,d.adrelid) AS expression FROM pg_attrdef d JOIN pg_attribute a ON a.attrelid=d.adrelid AND a.attnum=d.adnum WHERE d.adrelid='native_range_defaults'::regclass ORDER BY a.attnum", &[]).unwrap();
    assert_eq!(catalog.rows.len(), cases.len());
    for (row, case) in catalog.rows.iter().zip(cases) {
        let Value::Str(actual) = &row["node"] else {
            panic!("catalog node text")
        };
        // PostgreSQL prints bytes through native C char: Linux arm64 is unsigned, macOS is signed.
        // Compare every parsed field and byte, preserving all identities, lengths and flags.
        assert_eq!(
            uqa_sql::catalog::node_tree::parse(actual).unwrap(),
            uqa_sql::catalog::node_tree::parse(case["node"].as_str().unwrap()).unwrap(),
            "{}",
            case["id"]
        );
        assert_eq!(
            row["expression"],
            Value::Str(case["sql"].as_str().unwrap().into()),
            "{}",
            case["id"]
        );
    }
    let columns = (0..cases.len())
        .map(|index| format!("c{index}::text AS c{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let result = engine
        .sql(&format!("SELECT {columns} FROM native_range_defaults"), &[])
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    for (index, case) in cases.iter().enumerate() {
        assert_eq!(
            result.rows[0][&format!("c{index}")],
            Value::Str(case["output"].as_str().unwrap().into()),
            "{}",
            case["id"]
        );
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn native_range_retained_consumers_match_postgresql_after_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("range-consumers.db");
    let engine = super::range_input::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/range_datum_consumers_oracle.expected.json"
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
        super::range_input::open(provider, &path)
    };
    let mut durable = reference;
    durable["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    assert_eq!(durable["cases"].as_array().unwrap().len(), 12);
    crate::pg18_oracle::verify(&engine, &durable.to_string());
}
