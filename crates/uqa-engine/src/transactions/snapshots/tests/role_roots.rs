//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{capture::capture, *};

#[test]
fn committed_role_roots_preserve_original_memberships_with_bounded_capture_work() {
    for count in [1, 32, 128] {
        let engine = Engine::new();
        sql(&engine, "CREATE ROLE team");
        for index in 0..count {
            sql(&engine, &format!("CREATE ROLE saved_{index:03} LOGIN CONNECTION LIMIT 7; GRANT team TO saved_{index:03} WITH ADMIN OPTION"));
        }
        let original_roles = engine.durable.roles.snapshot();
        let original_memberships = engine.durable.role_memberships.snapshot();
        let expected_roles = original_roles
            .iter()
            .map(|(name, role)| (name.clone(), role.clone()))
            .collect::<Vec<_>>();
        let expected_memberships = original_memberships
            .iter()
            .map(|(key, edge)| (*key, edge.clone()))
            .collect::<Vec<_>>();
        let weak_roles = Arc::downgrade(&original_roles);
        let weak_memberships = Arc::downgrade(&original_memberships);
        drop((original_roles, original_memberships));
        sql(&engine, "BEGIN; ALTER ROLE saved_000 NOLOGIN CONNECTION LIMIT 3; REVOKE ADMIN OPTION FOR team FROM saved_000; SAVEPOINT changed");
        let control = StorageReadControl::with_limit(8 << 20);
        let selected = capture(&engine, &control);
        let mut captured = None;
        let allocations = allocation_counter::measure(|| {
            captured = Some((
                selected.catalogs.roles.captured(&selected.cut).unwrap(),
                selected
                    .catalogs
                    .role_memberships
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
        let (roles, memberships) = captured.unwrap();
        assert!(roles
            .get()
            .unwrap()
            .iter()
            .eq(expected_roles.iter().map(|(k, v)| (k, v))));
        assert!(memberships
            .get()
            .unwrap()
            .iter()
            .eq(expected_memberships.iter().map(|(k, v)| (k, v))));
        sql(&engine, "REVOKE team FROM saved_000; ROLLBACK TO changed; COMMIT; REVOKE team FROM saved_000; DROP ROLE saved_000; CREATE ROLE saved_000; ALTER ROLE team RENAME TO moved");
        drop(engine);
        assert!(weak_roles.upgrade().is_none());
        assert!(weak_memberships.upgrade().is_none());
        assert!(roles
            .get()
            .unwrap()
            .iter()
            .eq(expected_roles.iter().map(|(k, v)| (k, v))));
        assert!(memberships
            .get()
            .unwrap()
            .iter()
            .eq(expected_memberships.iter().map(|(k, v)| (k, v))));
        drop(selected);
        control.cancellation().cancel();
        assert!(roles.get().is_err());
        assert!(memberships.get().is_err());
        drop((roles, memberships));
        assert_eq!(control.memory().used(), 0);
    }
}
