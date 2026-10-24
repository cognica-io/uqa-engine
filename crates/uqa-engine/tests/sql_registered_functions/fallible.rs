//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_engine::SQLAggregateFactory;

#[derive(Default)]
struct Counts {
    attempts: AtomicUsize,
    fail_at: AtomicUsize,
    observations: AtomicUsize,
    finishes: AtomicUsize,
    drops: AtomicUsize,
}

struct Factory(Arc<Counts>);

impl SQLAggregateFactory for Factory {
    fn try_create_state(&self) -> Result<Box<dyn SQLAggregateState>, SQLError> {
        let attempt = self.0.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt == self.0.fail_at.load(Ordering::SeqCst) {
            return Err(SQLError::Diagnostic {
                sqlstate: "58000".into(),
                message: "host aggregate initialization failed".into(),
                detail: Some("state was not published".into()),
                hint: Some("retry after releasing host resources".into()),
            });
        }
        Ok(Box::new(State {
            total: 0,
            counts: Arc::clone(&self.0),
        }))
    }
}

struct State {
    total: i64,
    counts: Arc<Counts>,
}

impl SQLAggregateState for State {
    fn observe(&mut self, values: &[Value]) -> Result<(), SQLError> {
        let [Value::Int(value)] = values else {
            return Err(SQLError::TypeMismatch("expected one integer".into()));
        };
        self.total += value;
        self.counts.observations.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn finish(&self) -> Result<Value, SQLError> {
        self.counts.finishes.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Int(self.total))
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.counts.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

fn register(engine: &Engine) -> Arc<Counts> {
    let counts = Arc::new(Counts::default());
    engine
        .register_aggregate_function_with_options(
            "host_sum",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            Factory(Arc::clone(&counts)),
        )
        .unwrap();
    counts
}

fn assert_failure(error: &SQLError) {
    assert_eq!(error.sqlstate(), Some("58000"));
    assert_eq!(error.to_string(), "host aggregate initialization failed");
    assert_eq!(error.detail(), Some("state was not published"));
    assert_eq!(error.hint(), Some("retry after releasing host resources"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn creation_failure_aborts_the_native_frame_and_preserves_savepoint_recovery(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("aggregate.db"));
    let counts = register(&engine);
    engine
        .sql(
            "CREATE TABLE samples(g INTEGER, n INTEGER);
             INSERT INTO samples VALUES (1,1),(1,2),(2,3);
             CREATE TABLE markers(n INTEGER);
             PREPARE prepared_aggregate AS SELECT host_sum(n) FROM samples",
            &[],
        )
        .unwrap();
    assert_eq!(counts.attempts.load(Ordering::SeqCst), 0);

    for query in [
        "SELECT host_sum(n) FROM samples WHERE false",
        "SELECT host_sum(n ORDER BY n DESC) FROM samples",
        "SELECT host_sum(DISTINCT n) FROM samples",
        "SELECT g,host_sum(n) FROM samples GROUP BY g",
        "SELECT host_sum(n) FROM samples GROUP BY GROUPING SETS((),(g))",
        "SELECT host_sum(n) OVER(ORDER BY n) FROM samples",
        "EXECUTE prepared_aggregate",
    ] {
        let before = counts.attempts.load(Ordering::SeqCst);
        counts.fail_at.store(before + 1, Ordering::SeqCst);
        engine
            .sql(
                "BEGIN; INSERT INTO markers VALUES (1); SAVEPOINT before_factory;
                 INSERT INTO markers VALUES (2)",
                &[],
            )
            .unwrap();
        assert_failure(&engine.sql(query, &[]).unwrap_err());
        assert_eq!(
            counts.attempts.load(Ordering::SeqCst),
            before + 1,
            "{query}"
        );
        assert_eq!(counts.observations.load(Ordering::SeqCst), 0, "{query}");
        assert_eq!(counts.finishes.load(Ordering::SeqCst), 0, "{query}");
        assert_eq!(counts.drops.load(Ordering::SeqCst), 0, "{query}");
        assert!(engine.transaction_failed(), "{query}");
        assert_eq!(
            engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
            Some("25P02"),
        );
        engine.sql("ROLLBACK TO before_factory", &[]).unwrap();
        assert!(!engine.transaction_failed());
        assert_eq!(
            engine.sql("SELECT n FROM markers", &[]).unwrap().rows[0]["n"],
            Value::Int(1),
        );
        assert_eq!(
            engine.sql("SELECT n FROM markers", &[]).unwrap().rows.len(),
            1
        );
        engine.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(engine.transaction_depth(), 0);
    }

    counts.fail_at.store(0, Ordering::SeqCst);
    assert_eq!(
        engine.sql("EXECUTE prepared_aggregate", &[]).unwrap().rows[0]
            .values()
            .next(),
        Some(&Value::Int(6)),
    );
    assert_eq!(counts.observations.load(Ordering::SeqCst), 3);
    assert_eq!(counts.finishes.load(Ordering::SeqCst), 1);
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn empty_groups_and_window_frames_only_create_the_states_they_execute(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("aggregate.db"));
    engine
        .sql(
            "CREATE TABLE samples(g INTEGER, n INTEGER);
             INSERT INTO samples VALUES (1,1),(1,2),(2,3)",
            &[],
        )
        .unwrap();
    for (query, expected, creates, observations, finishes) in [
        ("SELECT host_sum(n) AS v FROM samples WHERE false", vec![0], 1, 0, 1),
        ("SELECT host_sum(n) AS v FROM samples WHERE false GROUP BY g", vec![], 0, 0, 0),
        ("SELECT host_sum(n) OVER() AS v FROM samples WHERE false", vec![], 0, 0, 0),
        ("SELECT host_sum(n) OVER(ORDER BY n ROWS UNBOUNDED PRECEDING) AS v FROM samples ORDER BY n", vec![1,3,6], 1, 3, 3),
        ("SELECT host_sum(n) OVER(PARTITION BY g ORDER BY n ROWS UNBOUNDED PRECEDING) AS v FROM samples ORDER BY n", vec![1,3,3], 2, 3, 3),
        ("SELECT host_sum(n) OVER(ORDER BY n ROWS CURRENT ROW) AS v FROM samples ORDER BY n", vec![1,2,3], 3, 3, 3),
        ("SELECT host_sum(n) OVER(ORDER BY n ROWS BETWEEN 1 FOLLOWING AND 1 FOLLOWING) AS v FROM samples WHERE n=1", vec![0], 1, 0, 1),
    ] {
        let counts = register(&engine);
        let result = engine.sql(query, &[]).unwrap();
        let values: Vec<_> = result.rows.iter().map(|row| row["v"].clone()).collect();
        assert_eq!(values, expected.into_iter().map(Value::Int).collect::<Vec<_>>(), "{query}");
        assert_eq!(counts.attempts.load(Ordering::SeqCst), creates, "{query}");
        assert_eq!(counts.observations.load(Ordering::SeqCst), observations, "{query}");
        assert_eq!(counts.finishes.load(Ordering::SeqCst), finishes, "{query}");
        assert_eq!(counts.drops.load(Ordering::SeqCst), creates, "{query}");
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn later_group_or_window_creation_failure_releases_all_earlier_states(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("aggregate.db"));
    engine
        .sql(
            "CREATE TABLE samples(n INTEGER); INSERT INTO samples VALUES (1),(2),(3)",
            &[],
        )
        .unwrap();
    for query in [
        "SELECT host_sum(n) FROM samples GROUP BY n",
        "SELECT host_sum(n) OVER(ORDER BY n ROWS CURRENT ROW) FROM samples",
    ] {
        let counts = register(&engine);
        counts.fail_at.store(2, Ordering::SeqCst);
        assert_failure(&engine.sql(query, &[]).unwrap_err());
        assert_eq!(counts.attempts.load(Ordering::SeqCst), 2);
        assert_eq!(counts.observations.load(Ordering::SeqCst), 1);
        assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
        assert_eq!(engine.transaction_depth(), 0);
        assert_eq!(
            engine
                .sql("SELECT count(*) AS n FROM samples", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(3)
        );
    }
}
