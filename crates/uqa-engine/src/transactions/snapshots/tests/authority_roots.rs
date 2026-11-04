//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[rstest::rstest]
#[case("COMMIT")]
#[case("ROLLBACK")]
fn committed_authority_roots_preserve_original_acl_revisions_with_bounded_capture_work(
    #[case] completion: &str,
) {
    for count in [1, 32, 128] {
        let engine = Engine::new();
        sql(
            &engine,
            "REVOKE EXECUTE ON FUNCTION lower(text) FROM PUBLIC",
        );
        for index in 0..count {
            sql(&engine, &format!("CREATE ROLE saved_{index:03}; GRANT EXECUTE ON FUNCTION lower(text) TO saved_{index:03} WITH GRANT OPTION; GRANT UPDATE ON pg_class TO saved_{index:03} WITH GRANT OPTION; GRANT SELECT(relname) ON pg_class TO saved_{index:03}"));
        }
        let original_routines = engine.durable.builtin_routine_security.snapshot();
        let original_systems = engine.durable.system_relation_security.snapshot();
        let expected_routines = original_routines
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect::<Vec<_>>();
        let expected_systems = original_systems
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>();
        let weak_routines = Arc::downgrade(&original_routines);
        let weak_systems = Arc::downgrade(&original_systems);
        drop((original_routines, original_systems));
        sql(&engine, "BEGIN; REVOKE EXECUTE ON FUNCTION lower(text) FROM saved_000; REVOKE UPDATE ON pg_class FROM saved_000; SAVEPOINT changed");
        let control = StorageReadControl::with_limit(8 << 20);
        let selected = capture(&engine, &control);
        let mut captured = None;
        let allocations = allocation_counter::measure(|| {
            captured = Some((
                selected
                    .catalogs
                    .builtin_routine_security
                    .captured(&selected.cut)
                    .unwrap(),
                selected
                    .catalogs
                    .system_relation_security
                    .captured(&selected.cut)
                    .unwrap(),
            ));
        });
        assert!(
            allocations.bytes_total < 32768,
            "count={count}: {allocations:?}"
        );
        assert!(
            allocations.count_total < 128,
            "count={count}: {allocations:?}"
        );
        let (routines, systems) = captured.unwrap();
        assert!(routines
            .get()
            .unwrap()
            .iter()
            .eq(expected_routines.iter().map(|(k, v)| (k, v))));
        assert!(systems
            .get()
            .unwrap()
            .iter()
            .eq(expected_systems.iter().map(|(k, v)| (k, v))));
        sql(
            &engine,
            "REVOKE SELECT(relname) ON pg_class FROM saved_000; ROLLBACK TO changed",
        );
        sql(&engine, completion);
        sql(&engine, "GRANT EXECUTE ON FUNCTION lower(text) TO PUBLIC; GRANT SELECT(relname) ON pg_class TO PUBLIC");
        assert_ne!(
            engine.durable.builtin_routine_security.read()[&870].revision,
            expected_routines[0].1.revision
        );
        let relation = RelationIdentity::new("pg_catalog", "pg_class");
        assert_ne!(
            engine.durable.system_relation_security.read()[&relation].columns["relname"].revision,
            expected_systems[0].1.columns["relname"].revision
        );
        drop(engine);
        assert!(weak_routines.upgrade().is_none());
        assert!(weak_systems.upgrade().is_none());
        assert!(routines
            .get()
            .unwrap()
            .iter()
            .eq(expected_routines.iter().map(|(k, v)| (k, v))));
        assert!(systems
            .get()
            .unwrap()
            .iter()
            .eq(expected_systems.iter().map(|(k, v)| (k, v))));
        drop(selected);
        control.cancellation().cancel();
        assert!(routines.get().is_err());
        assert!(systems.get().is_err());
        drop((routines, systems));
        assert_eq!(control.memory().used(), 0);
    }
}
