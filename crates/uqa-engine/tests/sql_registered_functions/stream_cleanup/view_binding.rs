//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn automatic_view_subqueries_bind_runtime_local_columns_before_outer_columns(
    #[case] provider: usize,
) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/cte_view_binding.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("view-scope.db"));
    engine
        .sql(reference["schema_setup"].as_str().unwrap(), &[])
        .unwrap();
    let mut failures = Vec::new();
    for case in reference["cases"].as_array().unwrap() {
        let query = case["query"].as_str().unwrap().replace("pg_temp.", "");
        let named_source = [("host_bad_rows", "bad"), ("host_alias_rows", "alias_n")]
            .into_iter()
            .find(|(name, _)| query.contains(name));
        let counts = if let Some((name, column)) = named_source {
            let counts = Arc::new(Counts::default());
            engine
                .register_table_function_with_options(
                    name,
                    SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                    Factory {
                        counts: counts.clone(),
                        behavior: Behavior {
                            shape: Shape::Named(column),
                            ..Behavior::default()
                        },
                        cancellation: engine.cancellation_token(),
                    },
                )
                .unwrap();
            counts
        } else {
            register(&engine, Behavior::default())
        };
        engine.sql("BEGIN; SAVEPOINT probe", &[]).unwrap();
        let result = engine.sql(&query, &[]);
        let observation = match result {
            Err(error)
                if case["sqlstate"]
                    .as_str()
                    .is_some_and(|state| error.sqlstate() == Some(state)) =>
            {
                None
            }
            Err(error) => Some(format!("{query}: {error:?}")),
            Ok(result) if !case["sqlstate"].is_null() => Some(format!(
                "{query}: expected SQLSTATE {}, received {:?}",
                case["sqlstate"], result.rows
            )),
            Ok(result) => {
                let mut actual = result
                    .rows
                    .iter()
                    .map(|row| {
                        assert_eq!(row.len(), 1);
                        match row.values().next().unwrap() {
                            Value::Int(value) => value.to_string(),
                            Value::Null => String::new(),
                            value => panic!("unexpected value for {query}: {value:?}"),
                        }
                    })
                    .collect::<Vec<_>>();
                let mut expected = case["returning"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().to_string())
                    .collect::<Vec<_>>();
                actual.sort();
                expected.sort();
                (actual != expected)
                    .then(|| format!("{query}: expected {expected:?}, received {actual:?}"))
            }
        };
        if let Some(failure) = observation {
            failures.push(failure);
        }
        let created = counts.created.load(Ordering::SeqCst);
        let closed = counts.closed.load(Ordering::SeqCst);
        let dropped = counts.dropped.load(Ordering::SeqCst);
        if created == 0 || closed != created || dropped != created {
            failures.push(format!(
                "{query}: created {created}, closed {closed}, dropped {dropped} scans"
            ));
        }
        engine.sql("ROLLBACK TO probe", &[]).unwrap();
        let sum = case["sum_after_savepoint_rollback"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            engine
                .sql("SELECT sum(n) AS n FROM cte_scope_target", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(sum),
            "{query}"
        );
        engine.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(engine.transaction_depth(), 0);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
