//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;

#[test]
fn role_registry_and_memberships_restore_together_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("roles.db");
    let (roles, memberships) = {
        let engine = Engine::open(&path).unwrap();
        engine.sql("CREATE ROLE creator CREATEROLE", &[]).unwrap();
        engine.sql("SET ROLE creator", &[]).unwrap();
        engine
            .sql("CREATE ROLE managed LOGIN CONNECTION LIMIT 3", &[])
            .unwrap();
        engine.sql("RESET ROLE", &[]).unwrap();
        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql("ALTER ROLE managed NOLOGIN CONNECTION LIMIT 7", &[])
            .unwrap();
        engine.sql("ROLLBACK", &[]).unwrap();
        let roles = engine.durable.roles.read().clone();
        let memberships = engine.durable.role_memberships.read().clone();
        assert_eq!(roles["managed"].connection_limit, 3);
        assert!(roles["managed"].has(uqa_sql::ast::RoleAttribute::Login));
        assert!(memberships
            .values()
            .any(|membership| membership.role == "managed"
                && membership.member == "creator"
                && membership.admin_option
                && !membership.inherit_option
                && !membership.set_option));
        (roles, memberships)
    };
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(*reopened.durable.roles.read(), roles);
    assert_eq!(*reopened.durable.role_memberships.read(), memberships);
}

#[test]
fn failed_role_declaration_preserves_live_registry_identity_and_prepared_cache() {
    let engine = Engine::new();
    engine.sql("CREATE ROLE limited", &[]).unwrap();
    engine.sql("SET ROLE limited", &[]).unwrap();
    engine
        .sql("PREPARE saved AS SELECT CURRENT_USER", &[])
        .unwrap();
    let roles = engine.durable.roles.snapshot();
    let memberships = engine.durable.role_memberships.snapshot();
    let prepared = engine.session.prepared.read()["saved"].logical_plan.clone();
    let error = engine
        .sql("CREATE ROLE forbidden SUPERUSER", &[])
        .unwrap_err();
    assert!(matches!(error, uqa_sql::SQLError::Routine { sqlstate, .. } if sqlstate == "42501"));
    assert!(std::sync::Arc::ptr_eq(
        &roles,
        &engine.durable.roles.snapshot()
    ));
    assert!(std::sync::Arc::ptr_eq(
        &memberships,
        &engine.durable.role_memberships.snapshot()
    ));
    assert!(std::sync::Arc::ptr_eq(
        &prepared,
        &engine.session.prepared.read()["saved"].logical_plan
    ));
    assert_eq!(engine.current_user_name(), "limited");
}
