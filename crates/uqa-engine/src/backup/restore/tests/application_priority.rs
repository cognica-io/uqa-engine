//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{external_functions::Total, providers::destination, *};
use crate::{tests::providers::Provider, SQLFunctionOptions, SQLTableFunctionResult};
use std::sync::atomic::{AtomicUsize, Ordering};

fn observe(engine: &Engine, sum: i64) {
    let value = |name: &str| {
        engine
            .sql(&format!("SELECT v FROM {name}"), &[])
            .unwrap()
            .rows[0]["v"]
            .clone()
    };
    assert_eq!(value("saved_sum"), Value::Int(sum));
    assert_eq!(value("saved_window_sum"), Value::Int(sum));
    assert_eq!(value("saved_rank"), Value::Int(1));
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_aggregates_follow_runtime_dispatch_and_ranking_uses_its_kernel(
    #[case] provider: Provider,
) {
    let files = tempfile::tempdir().unwrap();
    let store = repository(&files.path().join("archive"));
    let source = provider.open(&files.path().join("source"));
    source
        .sql(
            "CREATE VIEW saved_sum AS SELECT sum(n) AS v FROM (VALUES (1),(2)) t(n);
        CREATE VIEW saved_window_sum AS SELECT sum(n) OVER () AS v FROM (VALUES (1),(2)) t(n);
        CREATE VIEW saved_rank AS SELECT rank() OVER () AS v",
            &[],
        )
        .unwrap();
    observe(&source, 3);
    let mut sum = source
        .register_aggregate_function_retained("sum", SQLFunctionOptions::default(), || Total(17))
        .unwrap();
    source
        .register_aggregate_function("rank", || Total(99))
        .unwrap();
    observe(&source, 17);
    let parent = capture(&source, &store, None);
    sum.unregister().unwrap();
    observe(&source, 3);
    let child = capture(&source, &store, Some(parent.backup()));
    drop(source);

    let control = StorageReadControl::with_limit(32 << 20);
    let limits = RestoreOptions::new(16 << 20, 8 << 20);
    let authority = Engine::new();
    let unused = Engine::new();
    unused
        .register_scalar_function("sum", |_: &[Value]| Ok(Value::Int(41)))
        .unwrap();
    unused
        .register_table_function("sum", |_: &[Value]| {
            Ok(SQLTableFunctionResult::new(
                ["v"],
                vec![vec![Value::Int(43)]],
            ))
        })
        .unwrap();
    assert_unusable(
        authority
            .begin_restore(
                &store,
                parent.backup(),
                destination(provider, &files.path().join("missing")),
                limits.with_registered_functions(&unused),
                &control,
            )
            .unwrap(),
        "sum",
    );
    let current = Engine::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    current
        .register_aggregate_function("sum", move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Total(23)
        })
        .unwrap();
    for (root, runtime, name, expected) in [
        (parent, &current, "active", 23),
        (child, &unused, "retired", 3),
    ] {
        let mut job = authority
            .begin_restore(
                &store,
                root.backup(),
                destination(provider, &files.path().join(name)),
                limits.with_registered_functions(runtime),
                &control,
            )
            .unwrap();
        prepare(&mut job);
        let engine = job.publish().unwrap();
        if expected == 23 {
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        observe(&engine, expected);
    }
    assert_eq!(control.memory().used(), 0);
}
