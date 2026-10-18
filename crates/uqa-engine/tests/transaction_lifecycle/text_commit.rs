//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt::Write as _;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Barrier,
};
use std::time::{Duration, Instant};

use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage_sqlite::{ManagedConnection, SQLiteStorageProvider};

fn save(engine: &Engine, key: i64, body: &str) {
    engine.sql(
        "INSERT INTO preferences VALUES ($1, $2) ON CONFLICT (id) DO UPDATE SET body = excluded.body",
        &[SQLParam::scalar(Value::Int(key)), SQLParam::scalar(Value::Str(body.into()))],
    ).unwrap();
}

fn bodies(engine: &Engine) -> Vec<Value> {
    engine
        .sql("SELECT body FROM preferences ORDER BY id", &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|mut row| row.remove("body").unwrap())
        .collect()
}

fn commit_with_reader(writer: &Engine, before: &[Value], after: &[Value]) -> (Duration, Duration) {
    let reader = writer.new_session().unwrap();
    assert_eq!(bodies(&reader), before);
    let start = Arc::new(Barrier::new(2));
    let done = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        let start_reader = Arc::clone(&start);
        let done_reader = Arc::clone(&done);
        let reads = scope.spawn(move || {
            start_reader.wait();
            let mut maximum = Duration::ZERO;
            loop {
                let began = Instant::now();
                let rows = bodies(&reader);
                maximum = maximum.max(began.elapsed());
                assert!(
                    rows == before || rows == after,
                    "partially published text values"
                );
                if done_reader.load(Ordering::Acquire) {
                    break;
                }
            }
            assert_eq!(bodies(&reader), after);
            maximum
        });
        start.wait();
        let commit = Instant::now();
        writer.commit().unwrap();
        let commit_elapsed = commit.elapsed();
        done.store(true, Ordering::Release);
        (commit_elapsed, reads.join().unwrap())
    })
}

#[test]
fn native_text_upserts_publish_atomic_snapshots() {
    for (allowance, terms) in [(32 << 20, 64), (8 << 20, 4096)] {
        for indexed in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("text.db");
            let connection = ManagedConnection::open(&path).unwrap();
            connection
                .bind_native_records(VersionedSessionOptions {
                    retained_bytes: allowance,
                })
                .unwrap();
            let writer =
                Engine::from_persistent_provider(Arc::new(SQLiteStorageProvider::new(connection)))
                    .unwrap();
            writer
                .sql(
                    "CREATE TABLE preferences (id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
                    &[],
                )
                .unwrap();
            if indexed {
                writer
                    .sql(
                        "CREATE INDEX preferences_body ON preferences USING gin (body)",
                        &[],
                    )
                    .unwrap();
            }
            let mut original = String::new();
            for id in 0..terms {
                write!(original, "term{id:05} ").unwrap();
            }
            writer.begin().unwrap();
            for id in 1..=4 {
                save(&writer, id, &original);
            }
            writer.commit().unwrap();
            for changed in [0, 1, 4] {
                let before = bodies(&writer);
                let retained = writer.new_session().unwrap();
                retained
                    .sql("BEGIN ISOLATION LEVEL REPEATABLE READ", &[])
                    .unwrap();
                assert_eq!(bodies(&retained), before);
                let replacement = format!("{original} updated{changed}");
                let mut after = before.clone();
                writer.begin().unwrap();
                let statement = Instant::now();
                for id in 1..=changed.max(1) {
                    let value = if changed == 0 {
                        &original
                    } else {
                        &replacement
                    };
                    save(&writer, id, value);
                    after[(id - 1) as usize] = Value::Str(value.into());
                }
                let statement_elapsed = statement.elapsed();
                let (commit_elapsed, reader_elapsed) = commit_with_reader(&writer, &before, &after);
                eprintln!("terms={terms} indexed={indexed} changed={changed} statement={statement_elapsed:?} commit={commit_elapsed:?} reader={reader_elapsed:?}");
                assert_eq!(bodies(&writer), after);
                assert_eq!(bodies(&retained), before);
                retained.commit().unwrap();
                assert_eq!(bodies(&retained), after);
                if indexed {
                    let result = writer
                        .sql(
                            "SELECT count(*) AS n FROM preferences WHERE text_match(body, $1)",
                            &[SQLParam::scalar(Value::Str(format!("updated{changed}")))],
                        )
                        .unwrap();
                    assert_eq!(result.rows[0]["n"], Value::Int(changed));
                }
            }
            let committed = bodies(&writer);
            writer.begin().unwrap();
            save(&writer, 1, "discarded");
            writer.rollback().unwrap();
            assert_eq!(bodies(&writer), committed);
            writer.close().unwrap();
            let reopened = Engine::open(&path).unwrap();
            assert_eq!(
                bodies(&reopened),
                vec![Value::Str(format!("{original} updated4")); 4]
            );
        }
    }
}
