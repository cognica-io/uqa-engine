//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::query::locking::context::RowLockScopeSource;

#[rstest::rstest]
#[case::memory_direct(Provider::Memory, 0)]
#[case::memory_capability(Provider::Memory, 1)]
#[case::memory_catalog(Provider::Memory, 2)]
#[case::sqlite_native_direct(Provider::SQLiteNative, 0)]
#[case::sqlite_native_capability(Provider::SQLiteNative, 1)]
#[case::sqlite_native_catalog(Provider::SQLiteNative, 2)]
#[case::sqlite_kv_direct(Provider::SQLiteKeyValue, 0)]
#[case::sqlite_kv_capability(Provider::SQLiteKeyValue, 1)]
#[case::sqlite_kv_catalog(Provider::SQLiteKeyValue, 2)]
#[case::redb_direct(Provider::Redb, 0)]
#[case::redb_capability(Provider::Redb, 1)]
#[case::redb_catalog(Provider::Redb, 2)]
fn native_scope_factories_retain_request_admission_and_cancellation(
    #[case] provider: Provider,
    #[case] factory: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("routine-scope-factory.db"));
    engine
        .sql(
            "CREATE SCHEMA factory_scope; SET search_path = factory_scope, public",
            &[],
        )
        .unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let control = ProductionControl::new(memory, resources.cancellation(), runtime.cancellation);
    let capture = || match factory {
        0 => crate::capabilities::query_scope::new_for_current_routine(&engine),
        1 => engine.current_routine_scope(),
        _ => crate::capabilities::query_scope::new_for_catalog_binding(&engine),
    };
    drop(capture().unwrap());
    let before = memory.used();
    let scope = capture().unwrap();
    assert!(memory.used() > before);
    assert_eq!(
        scope
            .relation_name_resolution_with_control(&control)
            .unwrap()
            .search_path,
        ["factory_scope", "public"]
    );
    assert_eq!(
        scope
            .relation_name_resolution_with_control(&control)
            .unwrap()
            .lookup_mode,
        if factory == 2 {
            uqa_sql::catalog::resolution::RelationLookupMode::Bound
        } else {
            uqa_sql::catalog::resolution::RelationLookupMode::Dynamic
        }
    );
    drop(scope);
    assert_eq!(memory.used(), before);
    let occupied = memory.reserve(memory.available() - 32).unwrap();
    let allocation = allocation_counter::measure(|| {
        assert_eq!(capture().err().unwrap().sqlstate(), Some("53200"));
    });
    assert!(
        allocation.bytes_total < 16 << 10,
        "{provider:?} factory={factory}: {allocation:?}"
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    for token in [resources.cancellation(), runtime.cancellation] {
        token.cancel();
        assert_eq!(capture().err().unwrap().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), before);
        token.reset();
    }
    drop(capture().unwrap());
    assert_eq!(memory.used(), before);
    engine.sql("SELECT 42", &[]).unwrap();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
