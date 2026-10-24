//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_engine::SQLRowSource;

#[path = "stream_cleanup/cte.rs"]
mod cte;
#[path = "stream_cleanup/view_binding.rs"]
mod view_binding;

#[derive(Default)]
struct Counts {
    created: AtomicUsize,
    read: AtomicUsize,
    closed: AtomicUsize,
    dropped: AtomicUsize,
}

#[derive(Clone, Default)]
enum Shape {
    #[default]
    Normal,
    NoColumns,
    WrongWidth,
    Named(&'static str),
}

#[derive(Clone, Default)]
struct Behavior {
    close_error: bool,
    read_error: Option<usize>,
    cancel: bool,
    shape: Shape,
}

struct Factory {
    counts: Arc<Counts>,
    behavior: Behavior,
    cancellation: uqa_core::CancellationToken,
}

fn failure(state: &str, message: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: state.into(),
        message: message.into(),
        detail: Some(format!("{message} detail")),
        hint: Some(format!("{message} hint")),
    }
}

impl SQLTableFunction for Factory {
    fn call_stream(&self, args: &[Value]) -> Result<SQLTableFunctionStream, SQLError> {
        let [Value::Int(count)] = args else {
            return Err(SQLError::TypeMismatch("expected row count".into()));
        };
        self.counts.created.fetch_add(1, Ordering::SeqCst);
        Ok(SQLTableFunctionStream::from_source(
            match self.behavior.shape {
                Shape::NoColumns => Vec::new(),
                Shape::Named(name) => vec![name],
                _ => vec!["n"],
            },
            Source {
                counts: self.counts.clone(),
                behavior: self.behavior.clone(),
                cancellation: self.cancellation.clone(),
                count: *count,
                next: 0,
                closed: false,
            },
        ))
    }
}

struct Source {
    counts: Arc<Counts>,
    behavior: Behavior,
    cancellation: uqa_core::CancellationToken,
    count: i64,
    next: usize,
    closed: bool,
}

impl SQLRowSource<Vec<Value>> for Source {
    fn next_row(&mut self) -> Result<Option<Vec<Value>>, SQLError> {
        assert!(!self.closed);
        self.counts.read.fetch_add(1, Ordering::SeqCst);
        if self.behavior.read_error == Some(self.next) {
            return Err(failure("58000", "host read failed"));
        }
        if i64::try_from(self.next).unwrap() >= self.count {
            return Ok(None);
        }
        if self.behavior.cancel {
            self.cancellation.cancel();
        }
        let row = if matches!(self.behavior.shape, Shape::WrongWidth) {
            vec![]
        } else {
            vec![Value::Int(i64::try_from(self.next).unwrap())]
        };
        self.next += 1;
        Ok(Some(row))
    }

    fn close(&mut self) -> Result<(), SQLError> {
        assert!(
            !std::mem::replace(&mut self.closed, true),
            "scan closed twice"
        );
        self.counts.closed.fetch_add(1, Ordering::SeqCst);
        if self.behavior.close_error {
            Err(failure("58030", "host close failed"))
        } else {
            Ok(())
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        assert!(self.closed, "scan dropped before close");
        self.counts.dropped.fetch_add(1, Ordering::SeqCst);
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

fn register(engine: &Engine, behavior: Behavior) -> Arc<Counts> {
    let counts = Arc::new(Counts::default());
    engine
        .register_table_function_with_options(
            "host_rows",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            Factory {
                counts: counts.clone(),
                behavior,
                cancellation: engine.cancellation_token(),
            },
        )
        .unwrap();
    counts
}

fn assert_closed(counts: &Counts, query: &str) {
    let created = counts.created.load(Ordering::SeqCst);
    assert!(created > 0, "{query}");
    assert_eq!(counts.closed.load(Ordering::SeqCst), created, "{query}");
    assert_eq!(counts.dropped.load(Ordering::SeqCst), created, "{query}");
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn table_scans_finalize_at_eof_and_early_stop_before_statement_success(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("stream.db"));
    for query in [
        "SELECT * FROM host_rows(0)",
        "SELECT * FROM host_rows(3)",
        "SELECT * FROM host_rows(100) LIMIT 1",
        "SELECT * FROM host_rows(100) WITH ORDINALITY LIMIT 1",
        "SELECT host_rows(100) LIMIT 1",
        "SELECT * FROM ROWS FROM(host_rows(100), host_rows(100)) LIMIT 1",
        "SELECT * FROM (VALUES (100),(101)) v(n) CROSS JOIN LATERAL host_rows(v.n) LIMIT 1",
        "SELECT * FROM (VALUES (100),(101)) v(n) CROSS JOIN LATERAL (SELECT * FROM host_rows(v.n)) r LIMIT 1",
        "SELECT * FROM host_rows(3) ORDER BY n",
        "SELECT count(*) FROM host_rows(3)",
        "SELECT * FROM host_rows(100) LIMIT 0",
    ] {
        for close_error in [false, true] {
            let counts = register(&engine, Behavior { close_error, ..Behavior::default() });
            let result = engine.sql(query, &[]);
            if close_error {
                let error = result.unwrap_err();
                assert_eq!(error.sqlstate(), Some("58030"), "{query}: {error:?}");
                assert_eq!(error.to_string(), "host close failed", "{query}");
                assert_eq!(error.hint(), Some("host close failed hint"));
            } else { result.unwrap_or_else(|error| panic!("{query}: {error:?}")); }
            assert_closed(&counts, query);
            assert_eq!(engine.transaction_depth(), 0);
            engine.sql("SELECT 1", &[]).unwrap();
        }
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn table_scan_failures_keep_the_primary_sqlstate_and_cleanup_diagnostic(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("stream.db"));
    for (query, behavior, state) in [
        (
            "SELECT * FROM host_rows(3)",
            Behavior {
                read_error: Some(2),
                ..Behavior::default()
            },
            "58000",
        ),
        ("SELECT 1/n FROM host_rows(3)", Behavior::default(), "22012"),
        (
            "SELECT * FROM host_rows(3)",
            Behavior {
                shape: Shape::NoColumns,
                ..Behavior::default()
            },
            "42804",
        ),
        (
            "SELECT * FROM host_rows(3)",
            Behavior {
                shape: Shape::WrongWidth,
                ..Behavior::default()
            },
            "42804",
        ),
        (
            "SELECT * FROM host_rows(3)",
            Behavior {
                cancel: true,
                ..Behavior::default()
            },
            "57014",
        ),
        (
            "SELECT host_rows(3), 1/host_rows(3)",
            Behavior::default(),
            "22012",
        ),
        (
            "SELECT * FROM ROWS FROM(host_rows(3), host_rows(NULL::integer))",
            Behavior::default(),
            "42804",
        ),
        (
            "SELECT host_rows(3), host_rows(NULL::integer)",
            Behavior::default(),
            "42804",
        ),
    ] {
        let counts = register(
            &engine,
            Behavior {
                close_error: true,
                ..behavior
            },
        );
        let error = engine.sql(query, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{query}: {error:?}");
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("host close failed"),
            "{query}: {error:?}"
        );
        assert!(
            error
                .detail()
                .unwrap_or_default()
                .contains("host close failed hint"),
            "{query}: {error:?}"
        );
        assert_closed(&counts, query);
        assert_eq!(engine.transaction_depth(), 0);
        if state == "57014" {
            assert!(engine.is_cancelled());
            // Explicit host cancellation stays set until its owner withdraws it.
            engine.reset_cancellation();
        }
        engine.sql("SELECT 1", &[]).unwrap();
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn scan_cleanup_failure_reaches_prepared_and_cursor_transaction_boundaries(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("stream.db"));
    engine
        .sql(
            "CREATE TABLE retained(n INTEGER); INSERT INTO retained VALUES (1)",
            &[],
        )
        .unwrap();
    register(
        &engine,
        Behavior {
            close_error: true,
            ..Behavior::default()
        },
    );
    engine
        .sql("PREPARE scan AS SELECT * FROM host_rows(100) LIMIT 1", &[])
        .unwrap();
    engine
        .sql(
            "BEGIN; SAVEPOINT before_scan; INSERT INTO retained VALUES (2)",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.sql("EXECUTE scan", &[]).unwrap_err().sqlstate(),
        Some("58030")
    );
    assert_eq!(
        engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
        Some("25P02")
    );
    engine.sql("ROLLBACK TO before_scan; COMMIT", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM retained", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );

    let counts = register(
        &engine,
        Behavior {
            close_error: true,
            ..Behavior::default()
        },
    );
    let Err(error) = engine.sql_cursor("SELECT * FROM host_rows(3)", &[]) else {
        panic!("materialized cursor must report scan cleanup before success");
    };
    assert_eq!(error.sqlstate(), Some("58030"));
    assert_closed(&counts, "cursor");
    let error = engine
        .sql(
            "INSERT INTO retained VALUES (3); SELECT * FROM host_rows(3)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("58030"));
    assert_eq!(
        engine
            .sql("SELECT count(*) AS n FROM retained", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn construction_failure_closes_created_scans_before_returning_the_primary_error(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("construction.db"));
    for (query, state, created) in [
        ("SELECT missing FROM host_rows(3)", "42703", 1),
        ("SELECT * FROM host_rows(3) LIMIT -1", "2201W", 1),
        ("SELECT * FROM host_rows(3) OFFSET -1", "2201X", 1),
        ("SELECT * FROM host_rows(3) ORDER BY missing", "42703", 1),
        ("SELECT * FROM host_rows(3) WHERE missing > 0", "42703", 1),
        (
            "SELECT * FROM host_rows(3) a JOIN host_rows(NULL) b ON true",
            "42804",
            1,
        ),
        (
            "SELECT * FROM host_rows(3) a JOIN host_rows(3) b ON a.missing = b.n",
            "42703",
            2,
        ),
        (
            "SELECT * FROM (host_rows(3) a CROSS JOIN host_rows(3) b) AS j(x,y,z)",
            "42P10",
            0,
        ),
    ] {
        for close_error in [false, true] {
            let counts = register(
                &engine,
                Behavior {
                    close_error,
                    ..Behavior::default()
                },
            );
            engine.sql("BEGIN; SAVEPOINT before_build", &[]).unwrap();
            let error = engine.sql(query, &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some(state), "{query}: {error:?}");
            assert_eq!(counts.created.load(Ordering::SeqCst), created, "{query}");
            assert_eq!(counts.closed.load(Ordering::SeqCst), created, "{query}");
            assert_eq!(counts.dropped.load(Ordering::SeqCst), created, "{query}");
            assert_eq!(counts.read.load(Ordering::SeqCst), 0, "{query}");
            assert_eq!(
                error
                    .detail()
                    .unwrap_or_default()
                    .contains("host close failed"),
                close_error && created != 0,
                "{query}: {error:?}"
            );
            assert_eq!(
                engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
                Some("25P02")
            );
            engine.sql("ROLLBACK TO before_build; COMMIT", &[]).unwrap();
            engine.sql("SELECT 1", &[]).unwrap();
        }
    }
}
