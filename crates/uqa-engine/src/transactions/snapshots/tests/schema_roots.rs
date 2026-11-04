//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[test]
fn committed_schema_roots_preserve_original_authority_with_bounded_capture_work() {
    for count in [1, 32, 128] {
        let engine = Engine::new();
        sql(&engine, "CREATE ROLE reader");
        for index in 0..count {
            sql(&engine, &format!("CREATE SCHEMA saved_{index:03}"));
        }
        let original = engine.durable.schemas.snapshot();
        let expected = original
            .iter()
            .map(|(name, security)| (name.clone(), security.clone()))
            .collect::<Vec<_>>();
        let weak = Arc::downgrade(&original);
        drop(original);
        sql(
            &engine,
            "BEGIN; GRANT USAGE ON SCHEMA saved_000 TO reader WITH GRANT OPTION; SAVEPOINT changed",
        );
        let control = StorageReadControl::with_limit(4 << 20);
        let selected = capture(&engine, &control);
        let mut captured = None;
        let allocations = allocation_counter::measure(|| {
            captured = Some(selected.catalogs.schemas.captured(&selected.cut).unwrap());
        });
        assert!(
            allocations.bytes_total < 16384,
            "count={count}: {allocations:?}"
        );
        assert!(
            allocations.count_total < 64,
            "count={count}: {allocations:?}"
        );
        let captured = captured.unwrap();
        assert!(captured
            .get()
            .unwrap()
            .iter()
            .eq(expected.iter().map(|(name, security)| (name, security))));
        sql(&engine, "REVOKE USAGE ON SCHEMA saved_000 FROM reader; ROLLBACK TO changed; COMMIT; ALTER SCHEMA saved_000 RENAME TO moved; DROP SCHEMA moved; CREATE SCHEMA saved_000");
        drop(engine);
        assert!(weak.upgrade().is_none());
        assert!(captured
            .get()
            .unwrap()
            .iter()
            .eq(expected.iter().map(|(name, security)| (name, security))));
        drop(selected);
        control.cancellation().cancel();
        assert!(captured.get().is_err());
        drop(captured);
        assert_eq!(control.memory().used(), 0);
    }
}
