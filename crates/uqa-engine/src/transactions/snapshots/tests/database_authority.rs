//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn committed_database_authority_retains_original_grants_with_bounded_capture_work(
    #[case] completion: &str,
) {
    let mut first_capture_allocation = None;
    for count in [1, 32, 128] {
        let engine = Engine::new();
        for index in 0..count {
            sql(&engine, &format!("CREATE ROLE saved_{index:03}; GRANT CREATE ON DATABASE uqa TO saved_{index:03} WITH GRANT OPTION"));
        }
        let original = engine.durable.database_security.snapshot();
        let expected = original.as_ref().as_ref().clone();
        let weak = Arc::downgrade(&original);
        drop(original);
        sql(
            &engine,
            "BEGIN; REVOKE CREATE ON DATABASE uqa FROM saved_000; SAVEPOINT changed",
        );
        let control = StorageReadControl::with_limit(8 << 20);
        let selected = capture(&engine, &control);
        let mut saved = None;
        let allocations = allocation_counter::measure(|| {
            saved = Some(
                selected
                    .catalogs
                    .database_security
                    .captured(&selected.cut)
                    .unwrap(),
            );
        });
        assert!(
            allocations.bytes_total < 16384,
            "count={count}: {allocations:?}"
        );
        assert!(
            allocations.count_total < 64,
            "count={count}: {allocations:?}"
        );
        let allocation = (allocations.bytes_total, allocations.count_total);
        assert_eq!(
            *first_capture_allocation.get_or_insert(allocation),
            allocation
        );
        let saved = saved.unwrap();
        assert_eq!(saved.get().unwrap().as_ref(), &expected);
        sql(
            &engine,
            "GRANT CREATE ON DATABASE uqa TO PUBLIC; ROLLBACK TO changed",
        );
        sql(&engine, completion);
        sql(&engine, "GRANT CREATE ON DATABASE uqa TO PUBLIC");
        assert_ne!(engine.durable.database_security.read().as_ref(), &expected);
        drop(engine);
        assert!(weak.upgrade().is_none());
        assert_eq!(saved.get().unwrap().as_ref(), &expected);
        drop(selected);
        control.cancellation().cancel();
        assert!(saved.get().is_err());
        drop(saved);
        assert_eq!(control.memory().used(), 0);
    }
}
