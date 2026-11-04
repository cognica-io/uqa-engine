//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[test]
fn committed_wrapper_roots_preserve_original_options_and_incarnations_with_bounded_capture_work() {
    for count in [1, 32, 128] {
        let engine = Engine::new();
        for index in 0..count {
            sql(
                &engine,
                &format!(
                    "CREATE FOREIGN DATA WRAPPER saved_{index:03} OPTIONS (z 'first', a 'second')"
                ),
            );
        }
        let original = engine.durable.foreign_wrappers.snapshot();
        let expected = original
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Vec<_>>();
        let weak = Arc::downgrade(&original);
        drop(original);
        sql(&engine, "BEGIN; DROP FOREIGN DATA WRAPPER saved_000; CREATE FOREIGN DATA WRAPPER saved_000 HANDLER memory_fdw_handler OPTIONS (z 'private'); SAVEPOINT changed");
        let control = StorageReadControl::with_limit(8 << 20);
        let selected = capture(&engine, &control);
        let mut saved = None;
        let allocations = allocation_counter::measure(|| {
            saved = Some(
                selected
                    .catalogs
                    .foreign_wrappers
                    .captured(&selected.cut)
                    .unwrap(),
            );
        });
        assert!(
            allocations.bytes_total < 32768,
            "count={count}: {allocations:?}"
        );
        assert!(
            allocations.count_total < 128,
            "count={count}: {allocations:?}"
        );
        let saved = saved.unwrap();
        assert!(saved
            .get()
            .unwrap()
            .iter()
            .eq(expected.iter().map(|(k, v)| (k, v))));
        sql(&engine, "DROP FOREIGN DATA WRAPPER saved_000; ROLLBACK TO changed; COMMIT; DROP FOREIGN DATA WRAPPER saved_000; CREATE FOREIGN DATA WRAPPER saved_000");
        assert_ne!(
            saved.get().unwrap().get("saved_000").unwrap().identity,
            engine.durable.foreign_wrappers.read()["saved_000"].identity
        );
        drop(engine);
        assert!(weak.upgrade().is_none());
        assert!(saved
            .get()
            .unwrap()
            .iter()
            .eq(expected.iter().map(|(k, v)| (k, v))));
        drop(selected);
        control.cancellation().cancel();
        assert!(saved.get().is_err());
        drop(saved);
        assert_eq!(control.memory().used(), 0);
    }
}
