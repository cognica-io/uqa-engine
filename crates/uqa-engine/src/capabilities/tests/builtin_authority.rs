//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Builtin authority uses only the catalog inputs its ACL decision needs.

use super::{Engine, CATALOG_CAPTURES};
use std::sync::{atomic::Ordering, Arc};
use uqa_execution::catalog::security::builtin_routines::execution::BuiltinRoutinePermissions;
use uqa_sql::{ast::FunctionBinding, catalog::security::builtin_routines::BuiltinRoutineExecution};

fn lower() -> FunctionBinding {
    FunctionBinding {
        name: "pg_catalog.lower".into(),
        argument_types: vec!["text".into()],
        builtin: true,
        object_id: None,
        dispatch: None,
        invocation: None,
        composite_field: None,
        resolution_error: None,
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn unrestricted_builtin_authority_avoids_catalog_captures(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("builtin-authority.db");
    let engine = match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    engine.sql("CREATE ROLE reader", &[]).unwrap();
    for fillers in [0, 60] {
        for index in 0..fillers {
            engine
                .sql(
                    &format!("CREATE TABLE filler_{index}(id int PRIMARY KEY, body text)"),
                    &[],
                )
                .unwrap();
        }
        for role in ["uqa", "reader"] {
            engine.sql(&format!("SET ROLE {role}"), &[]).unwrap();
            CATALOG_CAPTURES.set(0);
            let authority = BuiltinRoutinePermissions::capture(&engine.catalog_execution());
            assert!(authority.unrestricted());
            authority.require_execute(&lower()).unwrap();
            assert_eq!(
                CATALOG_CAPTURES.get(),
                0,
                "unrestricted authority captured unrelated tables: fillers={fillers}, role={role}"
            );
            engine.sql("SELECT 1", &[]).unwrap();
            CATALOG_CAPTURES.set(0);
            let result = engine.sql("SELECT 1 AS value", &[]).unwrap();
            assert_eq!(result.rows[0]["value"], uqa_core::Value::Int(1));
            assert!(CATALOG_CAPTURES.get() <= 2, "plain query captured more than its analysis and execution catalogs: fillers={fillers}, role={role}");
        }
        engine.sql("RESET ROLE", &[]).unwrap();
    }
    engine
        .sql(
            "REVOKE EXECUTE ON FUNCTION pg_catalog.lower(text) FROM PUBLIC",
            &[],
        )
        .unwrap();
    CATALOG_CAPTURES.set(0);
    let superuser = BuiltinRoutinePermissions::capture(&engine.catalog_execution());
    assert!(superuser.unrestricted());
    superuser.require_execute(&lower()).unwrap();
    assert_eq!(CATALOG_CAPTURES.get(), 0);
    engine.sql("SET ROLE reader", &[]).unwrap();
    let restricted = BuiltinRoutinePermissions::capture(&engine.catalog_execution());
    assert!(!restricted.unrestricted());
    assert_eq!(
        restricted.require_execute(&lower()).unwrap_err().sqlstate(),
        Some("42501")
    );
}

#[test]
fn builtin_authority_preserves_retained_acl_role_and_rollback() {
    let mut engine = Engine::new();
    engine
        .sql("CREATE ROLE reader; SET ROLE reader", &[])
        .unwrap();
    let retained = Arc::new(crate::session::RetainedCatalogSnapshot {
        durable: engine.durable.snapshot(),
        read_view: engine.catalog_read_view(),
    });
    let allowed = BuiltinRoutinePermissions::capture(&engine.catalog_execution());
    engine.sql("RESET ROLE; REVOKE EXECUTE ON FUNCTION pg_catalog.lower(text) FROM PUBLIC; SET ROLE reader", &[]).unwrap();
    let revoked = BuiltinRoutinePermissions::capture(&engine.catalog_execution());
    assert_eq!(
        revoked.require_execute(&lower()).unwrap_err().sqlstate(),
        Some("42501")
    );
    allowed.require_execute(&lower()).unwrap();
    engine.query_catalog_snapshot = Some(retained);
    CATALOG_CAPTURES.set(0);
    let prior = BuiltinRoutinePermissions::capture(&engine.catalog_execution());
    prior.require_execute(&lower()).unwrap();
    assert_eq!(CATALOG_CAPTURES.get(), 0);
    engine.query_catalog_snapshot = None;
    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("RESET ROLE; GRANT EXECUTE ON FUNCTION pg_catalog.lower(text) TO reader; SET ROLE reader", &[]).unwrap();
    let granted = BuiltinRoutinePermissions::capture(&engine.catalog_execution());
    granted.require_execute(&lower()).unwrap();
    assert_eq!(
        engine.sql("SELECT lower('A') AS value", &[]).unwrap().rows[0]["value"],
        uqa_core::Value::Str("a".into())
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    let current = BuiltinRoutinePermissions::capture(&engine.catalog_execution());
    assert_eq!(
        current.require_execute(&lower()).unwrap_err().sqlstate(),
        Some("42501")
    );
    assert_eq!(
        engine.sql("SELECT lower('A')", &[]).unwrap_err().sqlstate(),
        Some("42501")
    );
    granted.require_execute(&lower()).unwrap();
    assert_eq!(
        revoked.require_execute(&lower()).unwrap_err().sqlstate(),
        Some("42501")
    );
}
