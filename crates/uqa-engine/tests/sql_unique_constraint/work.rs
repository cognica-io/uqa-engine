//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression uniqueness must do bounded work per staged row on every provider.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::Value;
use uqa_engine::{Engine, SQLFunctionOptions, SQLFunctionVolatility};

#[rstest::rstest]
#[case::memory(None)]
#[case::sqlite(Some(0))]
#[case::sqlite_key_value(Some(1))]
#[case::redb(Some(2))]
fn expression_unique_batch_evaluations_are_linear(#[case] provider: Option<u8>) {
    for count in [16, 64, 128] {
        let directory = tempfile::tempdir().unwrap();
        let engine = provider.map_or_else(Engine::new, |provider| {
            super::expressions::open_expression_engine(&directory.path().join("keys.db"), provider)
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        engine
            .register_scalar_function_with_options(
                "observe_key",
                SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable),
                move |arguments: &[Value]| {
                    observed.fetch_add(1, Ordering::Relaxed);
                    Ok(arguments[0].clone())
                },
            )
            .unwrap();
        engine.sql("CREATE FUNCTION expression_key(x integer) RETURNS integer LANGUAGE SQL IMMUTABLE AS $$ SELECT observe_key(x)::integer $$; CREATE TABLE items(id integer, body text); CREATE UNIQUE INDEX expression_unique ON items(expression_key(id))", &[]).unwrap();
        calls.store(0, Ordering::Relaxed);
        engine
            .sql(
                &format!("INSERT INTO items SELECT i, repeat('x',100) FROM generate_series(1,{count}) g(i)"),
                &[],
            )
            .unwrap();
        let evaluations = calls.load(Ordering::Relaxed);
        let rows = engine
            .sql("SELECT count(*) AS n, sum(id) AS total FROM items", &[])
            .unwrap();
        assert_eq!(rows.rows[0]["n"], Value::Int(count));
        assert_eq!(rows.rows[0]["total"], Value::Int(count * (count + 1) / 2));
        assert_eq!(
            engine
                .sql("INSERT INTO items VALUES(1, 'duplicate')", &[])
                .unwrap_err()
                .sqlstate(),
            Some("23505")
        );
        // Reservation, validation, staging and publication may evaluate the current key; none may reevaluate the complete staged prefix for every new row.
        assert!(
            evaluations <= usize::try_from(count).unwrap() * 6,
            "{count} rows evaluated their expression keys {evaluations} times"
        );
    }
}

#[rstest::rstest]
#[case::memory(None)]
#[case::sqlite(Some(0))]
#[case::sqlite_key_value(Some(1))]
#[case::redb(Some(2))]
fn staged_expression_keys_match_postgresql(#[case] provider: Option<u8>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("expression-keys.db");
    let engine = provider.map_or_else(Engine::new, |provider| {
        super::expressions::open_expression_engine(&path, provider)
    });
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/staged_expression_keys_oracle.expected.json"
    ))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    drop(engine);
    if let Some(provider) = provider {
        let ids = reference["reopen_ids"].as_array().unwrap().clone();
        reference["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| ids.contains(&case["id"]));
        crate::pg18_oracle::verify(
            &super::expressions::open_expression_engine(&path, provider),
            &reference.to_string(),
        );
    }
}
