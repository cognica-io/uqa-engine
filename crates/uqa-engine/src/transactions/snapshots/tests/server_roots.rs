//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[test]
fn committed_server_roots_preserve_original_options_and_incarnations_with_bounded_capture_work() {
    for count in [1, 32, 128] {
        let engine = Engine::new();
        for index in 0..count {
            sql(&engine, &format!("CREATE SERVER saved_{index:03} TYPE 'kind' VERSION '' FOREIGN DATA WRAPPER memory_fdw OPTIONS (z 'first', a 'second')"));
        }
        let original = engine.durable.foreign_servers.snapshot();
        let expected = original
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Vec<_>>();
        let weak = Arc::downgrade(&original);
        drop(original);
        sql(&engine, "BEGIN; DROP SERVER saved_000; CREATE SERVER saved_000 FOREIGN DATA WRAPPER memory_fdw OPTIONS (z 'private'); SAVEPOINT changed");
        let control = StorageReadControl::with_limit(8 << 20);
        let selected = capture(&engine, &control);
        let mut saved = None;
        let allocations = allocation_counter::measure(|| {
            saved = Some(
                selected
                    .catalogs
                    .foreign_servers
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
        sql(&engine, "DROP SERVER saved_000; ROLLBACK TO changed; COMMIT; DROP SERVER saved_000; CREATE SERVER saved_000 FOREIGN DATA WRAPPER memory_fdw");
        assert_ne!(
            saved
                .get()
                .unwrap()
                .get("saved_000")
                .unwrap()
                .metadata
                .object_id,
            engine.durable.foreign_servers.read()["saved_000"]
                .metadata
                .object_id
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
