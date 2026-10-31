//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::capabilities::query_scope;
use uqa_sql::catalog::roles::RoleReference;

fn verify_authority_capture(provider: Provider, command: bool) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("command-scope-capture.db"));
    engine.sql("CREATE TABLE items(id integer)", &[]).unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let subject = RoleReference::Named("x".repeat(4 << 20));
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let capture = || {
        if command {
            query_scope::new_for_command(&engine, Some(&subject), true)
        } else {
            query_scope::new_for_statement(&engine, Some(&subject))
        }
    };
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(capture());
    });
    assert!(
        allocation.bytes_total < 128 << 10,
        "{provider:?}, command={command}: {allocation:?}"
    );
    assert_eq!(result.unwrap().err().unwrap().sqlstate(), Some("53200"));
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    for token in [
        resources.cancellation(),
        engine.query_runtime_view().cancellation,
    ] {
        token.cancel();
        assert_eq!(capture().err().unwrap().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), baseline);
        token.reset();
    }
    let scope = capture().unwrap();
    assert_eq!(scope.privilege_subject().unwrap(), &subject);
    let resolution = scope.relation_name_resolution().unwrap();
    assert_eq!(
        resolution.lookup_mode,
        if command {
            uqa_sql::catalog::resolution::RelationLookupMode::Bound
        } else {
            uqa_sql::catalog::resolution::RelationLookupMode::Dynamic
        }
    );
    let retained = memory.used() - baseline;
    assert!(retained >= 4 << 20);
    let mut sibling = None;
    let cloned = allocation_counter::measure(|| {
        sibling = Some(scope.clone());
    });
    assert_eq!(cloned.count_total, 0, "{provider:?}: {cloned:?}");
    let sibling = sibling.unwrap();
    assert!(std::ptr::eq(
        scope.privilege_subject().unwrap(),
        sibling.privilege_subject().unwrap()
    ));
    drop(scope);
    // Producer ownership ends with the original scope; the copied authority and namespace remain shared.
    let retained = memory.used() - baseline;
    assert!(retained >= 4 << 20);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), retained);
    drop(sibling);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_command_scope_admits_authority_before_copy(#[case] provider: Provider) {
    verify_authority_capture(provider, true);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_statement_scope_admits_authority_before_copy(#[case] provider: Provider) {
    verify_authority_capture(provider, false);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_statement_scope_preserves_view_authority_current_user_portals_and_rollback(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("command-scope-authority.db");
    let engine = provider.open(&path);
    engine.sql("CREATE ROLE scope_capture_owner NOLOGIN; CREATE ROLE scope_capture_reader NOLOGIN; CREATE SCHEMA scope_capture_reference; GRANT USAGE,CREATE ON SCHEMA scope_capture_reference TO scope_capture_owner; GRANT USAGE ON SCHEMA scope_capture_reference TO scope_capture_reader; SET ROLE scope_capture_owner; CREATE TABLE scope_capture_reference.items(id integer); INSERT INTO scope_capture_reference.items VALUES(7); CREATE VIEW scope_capture_reference.visible AS SELECT id,current_user::text AS actor FROM scope_capture_reference.items; GRANT SELECT ON scope_capture_reference.visible TO scope_capture_reader; RESET ROLE; SET ROLE scope_capture_reader; SET search_path=scope_capture_reference", &[]).unwrap();
    let expected = vec![std::collections::BTreeMap::from([
        ("id".into(), Value::Int(7)),
        ("actor".into(), Value::Str("scope_capture_reader".into())),
    ])];
    engine
        .sql("PREPARE saved AS SELECT id,actor FROM visible", &[])
        .unwrap();
    for mode in ["force_generic_plan", "force_custom_plan"] {
        engine
            .sql(&format!("SET plan_cache_mode={mode}"), &[])
            .unwrap();
        assert_eq!(engine.sql("EXECUTE saved", &[]).unwrap().rows, expected);
    }
    engine
        .sql(
            "BEGIN; DECLARE scoped CURSOR FOR SELECT id,actor FROM visible",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.sql("FETCH ALL FROM scoped", &[]).unwrap().rows,
        expected
    );
    engine
        .sql("CLOSE scoped; SAVEPOINT before_denial", &[])
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT * FROM items", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    assert_eq!(
        engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
        Some("25P02")
    );
    engine.sql("ROLLBACK TO before_denial", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT id,actor FROM visible", &[])
            .unwrap()
            .rows,
        expected
    );
    engine.sql("COMMIT; EXPLAIN(MEMORY,FORMAT JSON) SELECT id,actor FROM visible; DEALLOCATE saved; RESET ROLE", &[]).unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let engine = provider.open(&path);
        assert_eq!(engine.sql("SET ROLE scope_capture_reader; SET search_path=scope_capture_reference; SELECT id,actor FROM visible", &[]).unwrap().rows, expected);
        engine.close().unwrap();
    }
}
