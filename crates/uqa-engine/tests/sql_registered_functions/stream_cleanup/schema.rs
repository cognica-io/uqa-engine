//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Pair {
    columns: [String; 2],
    declared: bool,
    changed: bool,
    close_error: bool,
    counts: Arc<Counts>,
}

impl SQLTableFunction for Pair {
    fn output_columns(&self) -> Option<&[String]> {
        self.declared.then_some(&self.columns)
    }
    fn call_stream(&self, arguments: &[Value]) -> Result<SQLTableFunctionStream, SQLError> {
        let [Value::Int(count)] = arguments else {
            panic!("integer argument: {arguments:?}")
        };
        let index = self.counts.created.fetch_add(1, Ordering::SeqCst);
        let columns = if self.changed && index != 0 {
            ["changed".into(), "label".into()]
        } else {
            self.columns.clone()
        };
        Ok(SQLTableFunctionStream::from_source(
            columns,
            Rows {
                counts: self.counts.clone(),
                next: 0,
                count: *count,
                close_error: self.close_error,
                closed: false,
            },
        ))
    }
}

struct Rows {
    counts: Arc<Counts>,
    next: i64,
    count: i64,
    close_error: bool,
    closed: bool,
}

impl SQLRowSource<Vec<Value>> for Rows {
    fn next_row(&mut self) -> Result<Option<Vec<Value>>, SQLError> {
        if self.next >= self.count {
            return Ok(None);
        }
        self.counts.read.fetch_add(1, Ordering::SeqCst);
        let row = vec![
            Value::Int(self.next),
            Value::Str(format!("row-{}", self.next)),
        ];
        self.next += 1;
        Ok(Some(row))
    }
    fn close(&mut self) -> Result<(), SQLError> {
        assert!(!self.closed);
        self.closed = true;
        self.counts.closed.fetch_add(1, Ordering::SeqCst);
        if self.close_error {
            Err(failure("58030", "schema scan cleanup failed"))
        } else {
            Ok(())
        }
    }
}

impl Drop for Rows {
    fn drop(&mut self) {
        assert!(self.closed);
        self.counts.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

fn register_pair(engine: &Engine, declared: bool, changed: bool, close_error: bool) -> Arc<Counts> {
    let counts = Arc::new(Counts::default());
    engine
        .register_table_function_with_options(
            "host_pair",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            Pair {
                columns: ["n".into(), "label".into()],
                declared,
                changed,
                close_error,
                counts: counts.clone(),
            },
        )
        .unwrap();
    counts
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn host_table_descriptors_and_correlated_rows_match_postgresql(#[case] provider: usize) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/host_table_schema.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("schema.db"));
    for declared in [false, true] {
        for case in reference["cases"].as_array().unwrap() {
            if !declared && case["requires_declared"] == true {
                continue;
            }
            let counts = register_pair(&engine, declared, false, false);
            let query = case["query"].as_str().unwrap().replace("pg_temp.", "");
            let result = engine.sql(&query, &[]);
            let expected = &case["result"];
            if let Some(state) = expected["sqlstate"].as_str() {
                let error = result.unwrap_err();
                assert_eq!(error.sqlstate(), Some(state), "{query}: {error:?}");
            } else {
                let result = result
                    .unwrap_or_else(|error| panic!("declared={declared}: {query}: {error:?}"));
                assert_eq!(
                    serde_json::json!(result.columns),
                    expected["columns"],
                    "{query}"
                );
                let rows = result
                    .rows
                    .iter()
                    .map(|row| {
                        result
                            .columns
                            .iter()
                            .map(|column| match &row[column] {
                                Value::Null => None,
                                Value::Int(value) => Some(value.to_string()),
                                Value::Str(value) => Some(value.clone()),
                                value => panic!("{query}: unexpected value {value:?}"),
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    serde_json::json!(rows),
                    expected["rows"],
                    "declared={declared}: {query}"
                );
            }
            assert_eq!(
                u64::try_from(counts.created.load(Ordering::SeqCst)).unwrap(),
                case["calls"].as_u64().unwrap(),
                "declared={declared}: {query}"
            );
            assert_eq!(
                counts.created.load(Ordering::SeqCst),
                counts.closed.load(Ordering::SeqCst),
                "{query}"
            );
            assert_eq!(
                counts.created.load(Ordering::SeqCst),
                counts.dropped.load(Ordering::SeqCst),
                "{query}"
            );
            if case["name"] == "bounded_first" {
                assert_eq!(counts.read.load(Ordering::SeqCst), 1);
            }
        }
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn runtime_descriptors_close_on_binding_failure_and_reject_changes(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("descriptor-errors.db"));
    for query in [
        "SELECT n FROM (VALUES(1),(2)) v(k), LATERAL host_pair(k)",
        "SELECT n FROM (VALUES(1),(2)) v(k), LATERAL (SELECT * FROM host_pair(k)) h",
    ] {
        let counts = register_pair(&engine, false, true, false);
        let error = engine.sql(query, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"), "{query}: {error:?}");
        assert_closed(&counts, query);
    }
    let counts = register_pair(&engine, false, false, true);
    engine.sql("BEGIN; SAVEPOINT before_shape", &[]).unwrap();
    let error = engine
        .sql(
            "SELECT * FROM (VALUES(1)) v(k) JOIN LATERAL host_pair(k) h ON h.missing=1",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42703"), "{error:?}");
    assert!(
        error
            .detail()
            .unwrap_or_default()
            .contains("schema scan cleanup failed"),
        "{error:?}"
    );
    assert_eq!(counts.read.load(Ordering::SeqCst), 0);
    assert_closed(&counts, "failed predicate binding");
    engine
        .sql("ROLLBACK TO before_shape; ROLLBACK", &[])
        .unwrap();
    let counts = register_pair(&engine, false, false, false);
    let error = engine
        .sql(
            "SELECT * FROM (SELECT 1 AS k WHERE FALSE) v, LATERAL host_pair(k)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P18"));
    assert_eq!(counts.created.load(Ordering::SeqCst), 0);
}
