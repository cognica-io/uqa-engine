//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[test]
fn committed_view_roots_retain_original_definitions_and_rows_with_bounded_capture_work() {
    for count in [1, 32, 128] {
        let engine = Engine::new();
        for index in 0..count {
            sql(
                &engine,
                &format!("CREATE VIEW saved_{index:03} AS SELECT {index} AS value"),
            );
        }
        sql(&engine, "CREATE MATERIALIZED VIEW evaluated AS SELECT repeat('x', 1048576) AS body, random() AS sampled; GRANT SELECT ON saved_000 TO PUBLIC");
        let original = engine.durable.views.snapshot();
        let expected = original
            .iter()
            .map(|(name, view)| (name.clone(), view.clone()))
            .collect::<Vec<_>>();
        let weak = original.identity();
        drop(original);
        sql(&engine, "BEGIN; DROP VIEW saved_000; CREATE VIEW saved_000 AS SELECT 999 AS value; REFRESH MATERIALIZED VIEW evaluated; SAVEPOINT changed");
        let control = StorageReadControl::with_limit(8 << 20);
        let selected = capture(&engine, &control);
        let mut saved = None;
        let allocations = allocation_counter::measure(|| {
            saved = Some(selected.catalogs.views.captured(&selected.cut).unwrap());
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
        sql(&engine, "DROP VIEW saved_000; ROLLBACK TO changed; COMMIT; DROP VIEW saved_000; CREATE VIEW saved_000 AS SELECT 1000 AS value");
        let relation = RelationIdentity::new("public", "saved_000");
        assert_ne!(
            saved.get().unwrap().get(&relation).unwrap().object_id,
            engine.durable.views.read()[&relation].object_id
        );
        drop(engine);
        assert!(!weak.is_alive());
        assert_eq!(saved.get().unwrap().len(), expected.len());
        for ((name, view), (old_name, old)) in saved.get().unwrap().iter().zip(&expected) {
            assert_eq!(name, old_name);
            assert_eq!(
                serde_json::to_value(view).unwrap(),
                serde_json::to_value(old).unwrap()
            );
            assert_eq!(view.security, old.security);
            for (row, original) in view.materialized_rows.iter().zip(&old.materialized_rows) {
                for ((column, value), (old_column, old_value)) in row.iter().zip(original) {
                    assert_eq!(column, old_column);
                    assert!(value.has_same_representation(old_value));
                }
            }
        }
        drop(selected);
        control.cancellation().cancel();
        assert!(saved.get().is_err());
        drop(saved);
        assert_eq!(control.memory().used(), 0);
    }
}
