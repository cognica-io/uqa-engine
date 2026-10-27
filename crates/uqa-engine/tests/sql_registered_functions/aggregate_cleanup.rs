//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct Counts {
    created: AtomicUsize,
    observed: AtomicUsize,
    finished: AtomicUsize,
    closed: Mutex<Vec<usize>>,
    dropped: AtomicUsize,
}

struct State {
    number: usize,
    counts: Arc<Counts>,
    failure: &'static str,
    cancellation: uqa_core::CancellationToken,
    total: i64,
}

fn failure(stage: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: if stage == "close" { "58000" } else { "P0001" }.into(),
        message: format!("host aggregate {stage} failed"),
        detail: Some(format!("{stage} detail")),
        hint: Some(format!("{stage} hint")),
    }
}

impl SQLAggregateState for State {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        self.counts.observed.fetch_add(1, Ordering::SeqCst);
        if self.failure == "observe" {
            return Err(failure("observe"));
        }
        if self.failure == "cancel" {
            self.cancellation.cancel();
        }
        if let [Value::Int(value)] = args {
            self.total += value;
        }
        Ok(())
    }

    fn finish(&self) -> Result<Value, SQLError> {
        self.counts.finished.fetch_add(1, Ordering::SeqCst);
        if self.failure == "finish" {
            Err(failure("finish"))
        } else {
            Ok(Value::Int(self.total))
        }
    }

    fn close(&mut self) -> Result<(), SQLError> {
        let mut closed = self.counts.closed.lock().unwrap();
        assert!(!closed.contains(&self.number), "state closed twice");
        closed.push(self.number);
        if self.failure == "none" {
            Ok(())
        } else {
            Err(failure("close"))
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

fn register(engine: &Engine, fail: &'static str, name: &str) -> Arc<Counts> {
    let counts = Arc::new(Counts::default());
    let states = Arc::clone(&counts);
    let cancellation = engine.cancellation_token();
    engine
        .register_aggregate_function_with_options(
            name,
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move || State {
                number: states.created.fetch_add(1, Ordering::SeqCst),
                counts: Arc::clone(&states),
                failure: fail,
                cancellation: cancellation.clone(),
                total: 0,
            },
        )
        .unwrap();
    counts
}

fn assert_released(counts: &Counts) -> usize {
    let created = counts.created.load(Ordering::SeqCst);
    assert_eq!(counts.closed.lock().unwrap().len(), created);
    assert_eq!(counts.dropped.load(Ordering::SeqCst), created);
    created
}

fn prepare(engine: &Engine) {
    engine
        .sql(
            "CREATE TABLE samples(g INTEGER, n INTEGER);
        INSERT INTO samples VALUES (1,1),(1,2),(2,3);
        CREATE TABLE markers(n INTEGER)",
            &[],
        )
        .unwrap();
}

#[derive(serde::Deserialize)]
struct Reference {
    version: String,
    cases: Vec<ReferenceCase>,
}

#[derive(serde::Deserialize)]
struct ReferenceCase {
    name: String,
    query: String,
    values: Vec<i64>,
    sqlstate: Option<String>,
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn explicit_state_cleanup_preserves_postgresql_group_and_window_results(#[case] provider: usize) {
    let reference: Reference = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/aggregate_state.expected.json"
    ))
    .unwrap();
    assert!(reference.version.starts_with("18."));
    let directory = tempfile::tempdir().unwrap();
    let engine = super::fallible::open(provider, &directory.path().join("aggregate.db"));
    prepare(&engine);
    for case in reference.cases {
        let counts = register(&engine, "none", "host_close");
        let query = case.query.replace("pg_temp.host_sum", "host_close");
        match engine.sql(&query, &[]) {
            Ok(result) => {
                assert!(case.sqlstate.is_none(), "{}", case.name);
                let values: Vec<_> = result.rows.iter().map(|row| row["v"].clone()).collect();
                assert_eq!(
                    values,
                    case.values.into_iter().map(Value::Int).collect::<Vec<_>>(),
                    "{}",
                    case.name
                );
            }
            Err(error) => assert_eq!(
                error.sqlstate(),
                case.sqlstate.as_deref(),
                "{}: {error}",
                case.name
            ),
        }
        assert_released(&counts);
        if case.name == "cumulative_window" {
            assert_eq!(counts.created.load(Ordering::SeqCst), 1);
            assert_eq!(counts.finished.load(Ordering::SeqCst), 3);
        }
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn state_close_failure_aborts_completion_and_allows_savepoint_recovery(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::fallible::open(provider, &directory.path().join("aggregate.db"));
    prepare(&engine);
    for query in [
        "SELECT host_close(n) FROM samples WHERE false",
        "SELECT host_close(n) FROM samples",
        "SELECT g,host_close(n) FROM samples GROUP BY g",
        "SELECT host_close(n) FROM samples GROUP BY GROUPING SETS((),(g),())",
        "SELECT host_close(n ORDER BY n DESC) FROM samples",
        "SELECT host_close(DISTINCT n) FROM samples",
        "SELECT host_close(n) FROM samples HAVING host_close(n) < 0",
        "SELECT host_close(n) OVER() FROM samples",
        "SELECT host_close(n) OVER(PARTITION BY g ORDER BY n) FROM samples",
        "SELECT host_close(n) OVER(ORDER BY n ROWS CURRENT ROW) FROM samples",
        "SELECT host_close(n) OVER(ORDER BY n ROWS BETWEEN 1 FOLLOWING AND 1 FOLLOWING) FROM samples WHERE n=1",
        "SELECT host_close(n) OVER(ORDER BY n ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW EXCLUDE CURRENT ROW) FROM samples",
    ] {
        let counts = register(&engine, "close", "host_close");
        engine.sql("BEGIN; INSERT INTO markers VALUES (1); SAVEPOINT before_close; INSERT INTO markers VALUES (2)", &[]).unwrap();
        let error = engine.sql(query, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("58000"), "{query}: {error}");
        assert_eq!(error.to_string(), "host aggregate close failed");
        assert_eq!(error.hint(), Some("close hint"));
        assert!(assert_released(&counts) > 0, "{query}");
        assert!(engine.transaction_failed());
        assert_eq!(engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(), Some("25P02"));
        engine.sql("ROLLBACK TO before_close", &[]).unwrap();
        assert!(!engine.transaction_failed());
        let result = engine.sql("SELECT n FROM markers", &[]).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0]["n"], Value::Int(1));
        engine.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(engine.transaction_depth(), 0);
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn primary_failure_closes_sibling_states_and_retains_all_diagnostics(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::fallible::open(provider, &directory.path().join("aggregate.db"));
    prepare(&engine);
    for (fail, query, primary) in [
        (
            "observe",
            "SELECT host_close(n),host_other(n) FROM samples",
            "P0001",
        ),
        (
            "finish",
            "SELECT host_close(n),host_other(n) FROM samples",
            "P0001",
        ),
        (
            "close",
            "SELECT host_close(n),host_other(10/(n-2)) FROM samples",
            "22012",
        ),
        (
            "close",
            "SELECT g,host_close(n),host_other(10/(n-2)) FROM samples GROUP BY g",
            "22012",
        ),
        (
            "observe",
            "SELECT host_close(n) FROM samples GROUP BY GROUPING SETS((),(g),())",
            "P0001",
        ),
        (
            "finish",
            "SELECT host_close(n) FROM samples GROUP BY GROUPING SETS((),(g),())",
            "P0001",
        ),
        (
            "finish",
            "SELECT host_close(n) OVER(),host_other(n) OVER() FROM samples",
            "P0001",
        ),
        (
            "finish",
            "SELECT host_other(n) OVER(),host_close(n) OVER() FROM samples",
            "P0001",
        ),
        (
            "observe",
            "SELECT host_close(DISTINCT n) FROM samples",
            "P0001",
        ),
        (
            "finish",
            "SELECT host_close(n ORDER BY n DESC) FROM samples",
            "P0001",
        ),
    ] {
        let first = register(&engine, fail, "host_close");
        let other = register(&engine, "close", "host_other");
        let error = engine.sql(query, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(primary), "{query}: {error}");
        assert!(
            error
                .detail()
                .unwrap()
                .contains("host aggregate close failed"),
            "{query}"
        );
        if primary == "P0001" {
            assert_eq!(error.hint(), Some(format!("{fail} hint")).as_deref());
        }
        let created = assert_released(&first);
        assert!(created > 0);
        let created = created + assert_released(&other);
        assert_eq!(
            error
                .detail()
                .unwrap()
                .matches("host aggregate close failed")
                .count(),
            created,
            "{query}"
        );
        assert_eq!(engine.transaction_depth(), 0);
        engine.sql("SELECT 1", &[]).unwrap();
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn state_cleanup_precedes_implicit_mutation_commit_and_prepared_completion(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::fallible::open(provider, &directory.path().join("aggregate.db"));
    prepare(&engine);
    let counts = register(&engine, "close", "host_close");
    engine.sql("PREPARE aggregate_write AS WITH added AS (INSERT INTO markers VALUES(8) RETURNING n) SELECT host_close(n) FROM added", &[]).unwrap();
    assert_eq!(counts.created.load(Ordering::SeqCst), 0);
    for query in [
        "EXECUTE aggregate_write",
        "INSERT INTO markers SELECT host_close(n) FROM samples",
        "WITH added AS (INSERT INTO markers VALUES(8) RETURNING n) SELECT host_close(n) OVER() FROM added",
    ] {
        let error = engine.sql(query, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("58000"), "{query}: {error}");
        assert_released(&counts);
        assert_eq!(engine.transaction_depth(), 0);
        assert_eq!(engine.sql("SELECT count(*) AS n FROM markers", &[]).unwrap().rows[0]["n"], Value::Int(0), "{query}");
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn cancellation_releases_retained_aggregate_state_before_returning(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = super::fallible::open(provider, &directory.path().join("aggregate.db"));
    let counts = register(&engine, "cancel", "host_close");
    let error = engine
        .sql(
            "SELECT host_close(n) FROM generate_series(1,4096) AS s(n)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"), "{error}");
    assert!(error
        .detail()
        .unwrap()
        .contains("host aggregate close failed"));
    assert!(assert_released(&counts) > 0);
    assert!(engine.is_cancelled());
    engine.reset_cancellation();
    assert_eq!(engine.transaction_depth(), 0);
    engine.sql("SELECT 1", &[]).unwrap();
}
