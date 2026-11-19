//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[test]
fn committed_type_roots_capture_in_bounded_work_without_temporary_payload_retention() {
    for count in [1, 32, 128] {
        let engine = Engine::new();
        let payload = "x".repeat(4096);
        for index in 0..count {
            sql(
                &engine,
                &format!("CREATE DOMAIN public.saved_{index:03} AS text DEFAULT '{payload}'"),
            );
        }
        let expected =
            serde_json::to_value(engine.durable.domains.read().iter().collect::<Vec<_>>()).unwrap();
        sql(&engine, "CREATE TEMP TABLE temporary_namespace(id integer)");
        sql(
            &engine,
            &format!(
                "CREATE DOMAIN pg_temp.large_default AS text DEFAULT '{}'",
                "t".repeat(2 << 20)
            ),
        );
        let original = engine.durable.domains.snapshot();
        let weak = original.identity();
        drop(original);
        sql(
            &engine,
            "BEGIN; CREATE DOMAIN private_type AS integer; SAVEPOINT original",
        );
        let control = StorageReadControl::with_limit(8 << 20);
        let selected = capture(&engine, &control);
        let mut captured = None;
        let allocations = allocation_counter::measure(|| {
            captured = Some(selected.catalogs.domains.captured(&selected.cut).unwrap());
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
        assert_eq!(captured.get().unwrap().len(), count);
        assert_eq!(
            serde_json::to_value(captured.get().unwrap()).unwrap(),
            expected
        );
        assert!(control.memory().used() >= count * payload.len());
        assert!(control.memory().used() < 2 << 20);
        sql(&engine, "ALTER DOMAIN saved_000 ADD CONSTRAINT changed CHECK(VALUE <> 'private') NOT VALID; ROLLBACK TO original; COMMIT; DROP DOMAIN saved_000; CREATE DOMAIN saved_000 AS integer");
        drop(engine);
        assert!(!weak.is_alive());
        assert_eq!(
            serde_json::to_value(captured.get().unwrap()).unwrap(),
            expected
        );
        drop(selected);
        control.cancellation().cancel();
        assert!(captured.get().is_err());
        drop(captured);
        assert_eq!(control.memory().used(), 0);
    }
}
