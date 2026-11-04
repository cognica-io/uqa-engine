//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{sql, Engine, StorageReadControl};
use std::collections::BTreeMap;
use uqa_sql::catalog::security::{
    system_relations::{CapturedSystemRelationSecurities, SystemRelationSecurities},
    table_binding::CapturedTableSecurity,
    BoundTableSecurity,
};
use uqa_storage::RelationIdentity;

fn fixture(engine: &Engine) {
    sql(engine, "CREATE ROLE reader; GRANT UPDATE ON pg_class TO reader WITH GRANT OPTION; GRANT SELECT(relname,relnamespace,relkind) ON pg_class TO reader; REVOKE EXECUTE ON FUNCTION lower(text) FROM PUBLIC; GRANT EXECUTE ON FUNCTION lower(text) TO reader WITH GRANT OPTION; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE \"Foreign.Name\"(v integer) SERVER remote; GRANT SELECT(v) ON \"Foreign.Name\" TO reader WITH GRANT OPTION");
}

fn same_system(saved: &CapturedSystemRelationSecurities, original: &SystemRelationSecurities) {
    assert_eq!(saved.len(), original.len());
    for ((name, value), (expected_name, expected)) in saved.iter().zip(original) {
        assert_eq!(name, expected_name);
        assert_eq!(value.table, expected.table);
        assert!(value.columns.iter().eq(expected.columns.iter()));
    }
}

fn same_foreign(
    saved: &[(RelationIdentity, CapturedTableSecurity)],
    original: &BTreeMap<RelationIdentity, BoundTableSecurity>,
) {
    assert_eq!(saved.len(), original.len());
    for ((name, value), (expected_name, expected)) in saved.iter().zip(original) {
        assert_eq!(name, expected_name);
        assert_eq!(value.privileges.role_owner, expected.role_owner);
        assert_eq!(value.privileges.acl(), expected.acl.as_deref());
        assert!(value
            .privileges
            .column_acls()
            .iter()
            .map(|(k, v)| (k, v))
            .eq(expected.column_acls.iter()));
        assert_eq!(value.relation_revision, expected.acl_revisions.relation);
        assert!(value
            .column_revisions
            .iter()
            .map(|(k, v)| (k, v))
            .eq(expected.acl_revisions.columns.iter()));
    }
}

#[test]
fn memory_relation_authority_capture_keeps_acl_revisions_through_rollback_and_replacement() {
    for mutate in [false, true] {
        let engine = Engine::new();
        fixture(&engine);
        let routines = engine.durable.builtin_routine_security.snapshot();
        let system = engine.durable.system_relation_security.snapshot();
        let foreign = engine.durable.foreign_table_security.snapshot();
        let routine_key = engine.durable.builtin_routine_security.key().unwrap();
        let system_key = engine.durable.system_relation_security.key().unwrap();
        let foreign_key = engine.durable.foreign_table_security.key().unwrap();
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
            sql(&engine, "BEGIN; REVOKE SELECT(relname) ON pg_class FROM reader; REVOKE EXECUTE ON FUNCTION lower(text) FROM reader; SAVEPOINT changed; DROP FOREIGN TABLE \"Foreign.Name\"; ROLLBACK TO changed; ROLLBACK");
            assert_eq!(*engine.durable.builtin_routine_security.read(), *routines);
            assert_eq!(*engine.durable.system_relation_security.read(), *system);
            assert_eq!(*engine.durable.foreign_table_security.read(), *foreign);
            sql(&engine, "REVOKE UPDATE ON pg_class FROM reader; REVOKE EXECUTE ON FUNCTION lower(text) FROM reader; DROP FOREIGN TABLE \"Foreign.Name\"; CREATE FOREIGN TABLE \"Foreign.Name\"(replacement text) SERVER remote");
        }
        drop(engine);
        let saved_routines = routine_key.retained(&cut).unwrap().unwrap();
        let saved_system = system_key.retained(&cut).unwrap().unwrap();
        let saved_foreign = foreign_key.retained(&cut).unwrap().unwrap();
        drop(cut);
        assert!(saved_routines.get().unwrap().iter().eq(routines.iter()));
        same_system(saved_system.get().unwrap(), &system);
        same_foreign(saved_foreign.get().unwrap(), &foreign);
        control.cancellation().cancel();
        assert!(saved_routines.get().is_err());
        assert!(saved_system.get().is_err());
        assert!(saved_foreign.get().is_err());
        drop((saved_routines, saved_system, saved_foreign));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn failed_relation_authority_capture_preserves_grants_and_other_capture_owners() {
    let engine = Engine::new();
    fixture(&engine);
    let original = engine.durable.system_relation_security.snapshot();
    let key = engine.durable.system_relation_security.key().unwrap();
    let small = StorageReadControl::with_limit(512);
    let good = StorageReadControl::with_limit(1 << 20);
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let failed = registry.capture(&small).unwrap();
    let cut = registry.capture(&good).unwrap();
    sql(&engine, "REVOKE SELECT(relname) ON pg_class FROM reader; REVOKE EXECUTE ON FUNCTION lower(text) FROM reader; REVOKE SELECT(v) ON \"Foreign.Name\" FROM reader");
    assert!(failed.check().is_err());
    assert_ne!(*engine.durable.system_relation_security.read(), *original);
    let saved = key.retained(&cut).unwrap().unwrap();
    same_system(saved.get().unwrap(), &original);
    drop((saved, cut, failed, engine));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}
