//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Declared {
    columns: Vec<String>,
    inner: Factory,
}

impl SQLTableFunction for Declared {
    fn output_columns(&self) -> Option<&[String]> {
        Some(&self.columns)
    }

    fn call_stream(&self, args: &[Value]) -> Result<SQLTableFunctionStream, SQLError> {
        self.inner.call_stream(args)
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn declared_table_columns_bind_empty_and_correlated_sources_without_calls(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("declared.db"));
    let counts = Arc::new(Counts::default());
    let register = |behavior| {
        engine
            .register_table_function_with_options(
                "host_rows",
                SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
                Declared {
                    columns: vec!["n".into()],
                    inner: Factory {
                        counts: counts.clone(),
                        behavior,
                        cancellation: engine.cancellation_token(),
                    },
                },
            )
            .unwrap();
    };
    register(Behavior::default());
    engine
        .register_scalar_function("host_arg_fail", |_: &[Value]| -> Result<Value, SQLError> {
            panic!("unconsumed argument must not be evaluated")
        })
        .unwrap();
    for (query, columns) in [
        (
            "SELECT * FROM host_rows(host_arg_fail()) LIMIT 0",
            vec!["n"],
        ),
        (
            "SELECT * FROM (SELECT 0 AS k WHERE FALSE) v, LATERAL host_rows(k)",
            vec!["k", "n"],
        ),
        (
            "SELECT * FROM (VALUES(1)) v(k), LATERAL host_rows(k) LIMIT 0",
            vec!["k", "n"],
        ),
        (
            "SELECT * FROM ROWS FROM(host_rows(host_arg_fail())) WITH ORDINALITY LIMIT 0",
            vec!["n", "ordinality"],
        ),
    ] {
        let result = engine
            .sql(query, &[])
            .unwrap_or_else(|error| panic!("{query}: {error:?}"));
        assert!(result.rows.is_empty(), "{query}");
        assert_eq!(result.columns, columns, "{query}");
        assert_eq!(counts.created.load(Ordering::SeqCst), 0, "{query}");
    }
    for query in [
        "SELECT n FROM (VALUES(2)) v(k), LATERAL host_rows(k)",
        "SELECT h.* FROM (VALUES(2)) v(k), LATERAL host_rows(k) h",
        "SELECT n FROM (VALUES(2)) v(k), LATERAL (SELECT * FROM host_rows(k)) h",
    ] {
        let result = engine
            .sql(query, &[])
            .unwrap_or_else(|error| panic!("{query}: {error:?}"));
        assert_eq!(result.columns, ["n"]);
        assert_eq!(
            result
                .rows
                .iter()
                .map(|row| row["n"].clone())
                .collect::<Vec<_>>(),
            [Value::Int(0), Value::Int(1)]
        );
        assert_closed(&counts, query);
    }
    let created = counts.created.load(Ordering::SeqCst);
    let error = engine
        .sql("SELECT missing FROM host_rows(2)", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42703"));
    assert_eq!(counts.created.load(Ordering::SeqCst), created);
    register(Behavior {
        shape: Shape::Named("changed"),
        close_error: true,
        ..Behavior::default()
    });
    engine.sql("BEGIN; SAVEPOINT before_call", &[]).unwrap();
    let error = engine.sql("SELECT * FROM host_rows(2)", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert!(error.to_string().contains("registered descriptor"));
    assert!(error.detail().unwrap().contains("host close failed"));
    assert_closed(&counts, "rejected descriptor");
    engine
        .sql("ROLLBACK TO before_call; ROLLBACK", &[])
        .unwrap();
}
