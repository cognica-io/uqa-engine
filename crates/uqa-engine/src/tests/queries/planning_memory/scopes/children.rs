//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::scalar::plan::{PhysicalOuterRow, PhysicalSubqueryRunner};

#[rstest::rstest]
#[case::memory_exists(Provider::Memory, 0)]
#[case::memory_scalar(Provider::Memory, 1)]
#[case::memory_rows(Provider::Memory, 2)]
#[case::sqlite_native_exists(Provider::SQLiteNative, 0)]
#[case::sqlite_native_scalar(Provider::SQLiteNative, 1)]
#[case::sqlite_native_rows(Provider::SQLiteNative, 2)]
#[case::sqlite_kv_exists(Provider::SQLiteKeyValue, 0)]
#[case::sqlite_kv_scalar(Provider::SQLiteKeyValue, 1)]
#[case::sqlite_kv_rows(Provider::SQLiteKeyValue, 2)]
#[case::redb_exists(Provider::Redb, 0)]
#[case::redb_scalar(Provider::Redb, 1)]
#[case::redb_rows(Provider::Redb, 2)]
fn native_child_scopes_admit_before_copying_and_recover_without_cached_failures(
    #[case] provider: Provider,
    #[case] operation: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("child-scopes.db"));
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
        uqa_sql::compile("WITH unused AS (SELECT 1) SELECT 42")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    query.ctes[0].name = "x".repeat(4 << 20);
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    scope.insert_deferred(query.ctes.remove(0).into());
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let before = memory.used();
    {
        let hook = crate::capabilities::ScopedEngineHook::new(&engine, &scope);
        let invoke = |slot| match operation {
            0 => hook
                .subquery_exists(slot, &query, PhysicalOuterRow::Absent, &[])
                .map(Value::Bool),
            1 => hook.scalar_subquery_value(slot, &query, PhysicalOuterRow::Absent, &[]),
            _ => hook
                .execute_subquery(slot, &query, PhysicalOuterRow::Absent, &[])
                .and_then(|result| {
                    let row = result.into_single_row()?.expect("one output row");
                    Ok(row.view().value_at(0).unwrap().clone())
                }),
        };
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let allocation = allocation_counter::measure(|| {
            assert_eq!(invoke(0).unwrap_err().sqlstate(), Some("53200"));
        });
        assert!(
            allocation.bytes_total < 256 << 10,
            "{provider:?} operation={operation}: {allocation:?}"
        );
        assert_eq!(memory.used(), before + occupied.bytes());
        drop(occupied);
        let runtime = engine.query_runtime_view();
        for token in [resources.cancellation(), runtime.cancellation] {
            token.cancel();
            assert_eq!(invoke(0).unwrap_err().sqlstate(), Some("57014"));
            assert_eq!(memory.used(), before);
            token.reset();
        }
        let expected = if operation == 0 {
            Value::Bool(true)
        } else {
            Value::Int(42)
        };
        assert_eq!(invoke(0).unwrap(), expected);
        assert_eq!(invoke(0).unwrap(), expected);
    }
    drop(scope);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
