//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{
    plan::{CommandPlan, ExecutablePlanOptimizer},
    SQLParam,
};

fn large_command() -> UnifiedPlan {
    UnifiedPlan::Command(Box::new(CommandPlan::SetVariable {
        name: "application_name".into(),
        value: "x".repeat(4 * 1024 * 1024),
        local: false,
        is_default: false,
    }))
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_analysis_cache_admits_utility_payloads_and_recovers(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("analysis-cache.db"));
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let before = memory.used();
    let sibling = memory.reserve(memory.available() - 16 * 1024).unwrap();
    let plan = large_command();
    let result = engine.plan_with_cached_analysis(plan, &[], None);
    let Err(error) = result else {
        panic!("cached utility payload escaped admission: {provider:?}");
    };
    assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    assert_eq!(memory.used(), before + sibling.bytes());
    drop(sibling);
    let (plan, cached) = engine
        .plan_with_cached_analysis(large_command(), &[], None)
        .unwrap();
    drop(plan);
    let cached = cached.unwrap();
    assert!(memory.used() >= before + 4 * 1024 * 1024);
    assert!(cached.plan_for(&[]).is_some());
    drop(cached);
    assert_eq!(memory.used(), before);
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_analysis_cache_matches_borrowed_types_before_output_admission(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("analysis-cache-match.db"));
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let before = memory.used();
    let params = [SQLParam::typed_scalar(
        Value::Null,
        uqa_sql::ColumnType::Named("t".repeat(64 * 1024)),
    )];
    let empty = || UnifiedPlan::Command(Box::new(CommandPlan::Empty));
    let (plan, cached) = engine
        .plan_with_cached_analysis(empty(), &params, None)
        .unwrap();
    drop(plan);
    let cached = cached.unwrap();
    let retained = memory.used();
    assert!(retained > before + 64 * 1024);
    let sibling = memory.reserve(memory.available()).unwrap();
    let plan = empty();
    let allocation = allocation_counter::measure(|| {
        let error = engine
            .plan_with_cached_analysis(plan, &params, Some(cached.clone()))
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    });
    assert!(
        allocation.bytes_total < 8192,
        "{provider:?}: {allocation:?}"
    );
    assert_eq!(memory.used(), retained + sibling.bytes());
    drop(sibling);
    let (plan, reused) = engine
        .plan_with_cached_analysis(empty(), &params, Some(cached.clone()))
        .unwrap();
    assert!(cached.ptr_eq(&reused.unwrap()));
    drop(plan);
    assert_eq!(memory.used(), retained);
    drop(cached);
    assert_eq!(memory.used(), before);
    engine.close().unwrap();
}
