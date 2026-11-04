//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use std::collections::BTreeMap;
use uqa_sql::catalog::roles::{capture::CapturedRoleDefinition, RoleDefinition};

fn same_roles(
    saved: &[(String, CapturedRoleDefinition)],
    expected: &BTreeMap<String, RoleDefinition>,
) {
    assert_eq!(saved.len(), expected.len());
    for ((name, role), (expected_name, original)) in saved.iter().zip(expected) {
        assert_eq!(name, expected_name);
        assert_eq!(role.oid, original.oid);
        assert_eq!(role.object_id, original.object_id);
        assert_eq!(role.revision, original.revision);
        assert_eq!(role.name, original.name);
        assert_eq!(role.connection_limit, original.connection_limit);
        assert!(role.attributes.iter().eq(original.attributes.iter()));
    }
}

#[test]
fn memory_role_capture_preserves_rollback_membership_and_same_name_replacement() {
    for mutate in [false, true] {
        let engine = Engine::new();
        sql(&engine, "CREATE ROLE team; CREATE ROLE member LOGIN CONNECTION LIMIT 7; GRANT team TO member WITH ADMIN OPTION");
        let roles = engine.durable.roles.snapshot();
        let memberships = engine.durable.role_memberships.snapshot();
        let role_key = engine.durable.roles.key().unwrap();
        let membership_key = engine.durable.role_memberships.key().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(control.memory().used() < 4096);
        if mutate {
            sql(&engine, "BEGIN; ALTER ROLE member CONNECTION LIMIT 3; SAVEPOINT changed; REVOKE team FROM member; ROLLBACK TO changed; ROLLBACK");
            assert_eq!(*engine.durable.roles.read(), *roles);
            assert_eq!(*engine.durable.role_memberships.read(), *memberships);
            sql(&engine, "REVOKE team FROM member; DROP ROLE member; CREATE ROLE member NOLOGIN; ALTER ROLE team RENAME TO renamed");
            assert_ne!(
                engine.durable.roles.read()["member"].object_id,
                roles["member"].object_id
            );
        }
        drop(engine);
        let saved_roles = role_key.retained(&cut).unwrap().unwrap();
        let saved_memberships = membership_key.retained(&cut).unwrap().unwrap();
        drop(cut);
        same_roles(saved_roles.get().unwrap(), &roles);
        assert!(saved_memberships
            .get()
            .unwrap()
            .iter()
            .map(|(k, v)| (k, v))
            .eq(memberships.iter()));
        control.cancellation().cancel();
        assert!(saved_roles.get().is_err());
        assert!(saved_memberships.get().is_err());
        drop((saved_roles, saved_memberships));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn underfunded_role_capture_does_not_abort_role_or_membership_changes() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE ROLE team; CREATE ROLE member; GRANT team TO member WITH ADMIN OPTION",
    );
    let original = engine.durable.roles.snapshot();
    let memberships = engine.durable.role_memberships.snapshot();
    let role_key = engine.durable.roles.key().unwrap();
    let membership_key = engine.durable.role_memberships.key().unwrap();
    let small = StorageReadControl::with_limit(512);
    let funded = StorageReadControl::with_limit(1 << 20);
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let failed = registry.capture(&small).unwrap();
    let cut = registry.capture(&funded).unwrap();
    sql(
        &engine,
        "ALTER ROLE member CONNECTION LIMIT 9; REVOKE team FROM member",
    );
    assert!(failed.check().is_err());
    assert_eq!(engine.durable.roles.read()["member"].connection_limit, 9);
    assert!(engine.durable.role_memberships.read().is_empty());
    let saved_roles = role_key.retained(&cut).unwrap().unwrap();
    let saved_memberships = membership_key.retained(&cut).unwrap().unwrap();
    same_roles(saved_roles.get().unwrap(), &original);
    assert!(saved_memberships
        .get()
        .unwrap()
        .iter()
        .map(|(k, v)| (k, v))
        .eq(memberships.iter()));
    drop((cut, failed, saved_roles, saved_memberships, engine));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(funded.memory().used(), 0);
}
