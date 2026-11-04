//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};
use uqa_sql::routines::{CompiledFunctionBody, RoutineBody};

#[test]
fn committed_routine_roots_preserve_original_bodies_and_authority_with_bounded_capture_work() {
    for count in [1, 32, 128] {
        let engine = Engine::new();
        for index in 0..count {
            sql(&engine, &format!("CREATE FUNCTION saved_{index:03}() RETURNS integer LANGUAGE SQL RETURN {index}"));
        }
        sql(&engine, "CREATE FUNCTION saved_000(a text) RETURNS text LANGUAGE SQL AS 'SELECT a'; REVOKE EXECUTE ON FUNCTION saved_000() FROM PUBLIC");
        let original = engine.durable.sql_user_functions.snapshot();
        let expected = original
            .iter()
            .map(|(name, values)| (name.clone(), values.clone()))
            .collect::<Vec<_>>();
        let weak = Arc::downgrade(&original);
        drop(original);
        sql(&engine, "BEGIN; DROP FUNCTION saved_000(); CREATE FUNCTION saved_000() RETURNS integer LANGUAGE SQL RETURN 999; GRANT EXECUTE ON FUNCTION saved_000(text) TO PUBLIC; SAVEPOINT changed");
        let control = StorageReadControl::with_limit(8 << 20);
        let selected = capture(&engine, &control);
        let mut saved = None;
        let allocations = allocation_counter::measure(|| {
            saved = Some(
                selected
                    .catalogs
                    .sql_user_functions
                    .captured(&selected.cut)
                    .unwrap(),
            );
        });
        assert!(
            allocations.bytes_total < 32768 && allocations.count_total < 128,
            "count={count}: {allocations:?}"
        );
        let saved = saved.unwrap();
        sql(&engine, "DROP FUNCTION saved_000(); ROLLBACK TO changed; COMMIT; DROP FUNCTION saved_000(); CREATE FUNCTION saved_000() RETURNS integer LANGUAGE SQL RETURN 1000");
        let original_identity = expected
            .iter()
            .find(|(name, _)| name == "public.saved_000")
            .unwrap()
            .1[0]
            .def
            .object_id;
        assert_ne!(
            engine.durable.sql_user_functions.read()["public.saved_000"][0]
                .def
                .object_id,
            original_identity
        );
        drop(engine);
        assert!(weak.upgrade().is_none());
        assert_eq!(saved.get().unwrap().len(), expected.len());
        for ((name, routines), (old_name, originals)) in saved.get().unwrap().iter().zip(&expected)
        {
            assert_eq!(name, old_name);
            assert_eq!(routines.len(), originals.len());
            for (routine, original) in routines.iter().zip(originals) {
                assert!(Arc::ptr_eq(routine, original));
                assert_eq!(
                    serde_json::to_value(&routine.def).unwrap(),
                    serde_json::to_value(&original.def).unwrap()
                );
                assert_eq!(routine.catalog_revision(), original.catalog_revision());
                match (&routine.body, &original.body) {
                    (RoutineBody::Source, RoutineBody::Source) => {}
                    (RoutineBody::Bound(body), RoutineBody::Bound(old_body)) => {
                        assert!(Arc::ptr_eq(body, old_body));
                        let (
                            CompiledFunctionBody::SQL(plans),
                            CompiledFunctionBody::SQL(old_plans),
                        ) = (&***body, &***old_body)
                        else {
                            panic!("bound SQL")
                        };
                        assert_eq!(
                            serde_json::to_value(plans).unwrap(),
                            serde_json::to_value(old_plans).unwrap()
                        );
                    }
                    _ => panic!("routine body changed"),
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
