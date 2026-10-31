//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;
use uqa_core::memory::MemoryBudget;

fn fixture(provider: Provider) -> (tempfile::TempDir, Engine) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("statistics-production.db"));
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    engine.sql("CREATE TABLE distribution (id integer, payload text); INSERT INTO distribution SELECT i, lpad(i::text, 1000, 'x') FROM generate_series(1, 128) AS i", &[]).unwrap();
    (directory, engine)
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_distribution_collection_rejects_before_large_output_and_recovers(
    #[case] provider: Provider,
) {
    let (_directory, engine) = fixture(provider);
    let retained = engine.query_retention_control().unwrap();
    let budget = retained.memory().child(16 * 1024);
    let columns = ["payload".to_owned()];
    let control = ProductionControl::new(
        &budget,
        retained.cancellation(),
        &engine.runtime.cancellation,
    );
    let error = engine
        .collect_hierarchy_analyze_statistics("public.distribution", &columns, true, control)
        .unwrap_err();
    let error = uqa_sql::catalog::errors::storage_error("test statistics admission", &error);
    assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    assert_eq!(budget.used(), 0);

    let control = ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        &engine.runtime.cancellation,
    );
    let (statistics, rows) = engine
        .collect_hierarchy_analyze_statistics("public.distribution", &columns, true, control)
        .unwrap();
    assert_eq!(rows, 128);
    assert_eq!(statistics["payload"].distinct_count, 128);
    assert_eq!(statistics["payload"].histogram.len(), 101);
    let before = retained.memory().used();
    assert!(
        before > 100 * 1000,
        "retained output must keep its original allowance"
    );
    let reader = statistics.clone();
    drop(statistics);
    assert_eq!(retained.memory().used(), before);
    drop(reader);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(retained.memory().used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_statistics_publication_retains_the_request_and_releases_replaced_generations(
    #[case] provider: Provider,
) {
    let (_directory, engine) = fixture(provider);
    let table = engine.try_table("public.distribution").unwrap().unwrap();
    let retained = engine.query_retention_control().unwrap();
    let initial = retained.memory().used();
    let full = retained
        .memory()
        .reserve(retained.memory().available())
        .unwrap();
    let error = engine
        .analyze_table("public.distribution", &table, false, None, true)
        .unwrap_err();
    assert_eq!(
        uqa_sql::catalog::errors::storage_error("test column selection", &error).sqlstate(),
        Some("53200")
    );
    drop(full);
    assert_eq!(retained.memory().used(), initial);
    engine.cancel();
    let error = engine
        .analyze_table("public.distribution", &table, false, None, true)
        .unwrap_err();
    assert_eq!(
        uqa_sql::catalog::errors::storage_error("test column cancellation", &error).sqlstate(),
        Some("57014")
    );
    engine.reset_cancellation();
    assert_eq!(retained.memory().used(), initial);
    engine
        .analyze_table("public.distribution", &table, false, None, true)
        .unwrap();
    let first = table.column_stats.read().clone();
    let before = retained.memory().used();
    assert!(before > 100 * 1000, "{provider:?}: {before}");
    engine
        .analyze_table(
            "public.distribution",
            &table,
            false,
            Some(&["id".into()]),
            true,
        )
        .unwrap();
    let second = table.column_stats.read().clone();
    assert_eq!(second["payload"].histogram, first["payload"].histogram);
    assert!(retained.memory().used() > before);
    drop(first);
    assert!(retained.memory().used() < 2 * before);
    drop(table);
    engine.close().unwrap();
    drop(engine);
    assert!(retained.memory().used() > 100 * 1000);
    drop(second);
    assert_eq!(retained.memory().used(), 0);
}

#[test]
fn memory_lazy_statistics_failure_preserves_catalog_and_retries_on_the_same_owner() {
    let (_directory, engine) = fixture(Provider::Memory);
    let table = engine.try_table("public.distribution").unwrap().unwrap();
    let retained = engine.query_retention_control().unwrap();
    let budget = retained.memory();
    let initial = budget.used();
    let full = budget.reserve(budget.available()).unwrap();
    let error = uqa_planner::statement_planning::PlannerStatisticsCatalog::column_statistics(
        &engine,
        "distribution",
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(table.column_stats_dirty.load(Ordering::Acquire));
    assert!(table.column_stats.read().is_empty());
    drop(full);
    assert_eq!(budget.used(), initial);
    let statistics = uqa_planner::statement_planning::PlannerStatisticsCatalog::column_statistics(
        &engine,
        "distribution",
    )
    .unwrap();
    assert_eq!(statistics["payload"].row_count, 128);
    assert!(!table.column_stats_dirty.load(Ordering::Acquire));
    drop((statistics, table));
    engine.close().unwrap();
    drop(engine);
    assert_eq!(budget.used(), 0);
}

#[test]
fn memory_lazy_statistics_cancelled_collection_keeps_the_old_snapshot() {
    let (_directory, engine) = fixture(Provider::Memory);
    let table = engine.try_table("public.distribution").unwrap().unwrap();
    let retained = engine.query_retention_control().unwrap();
    let budget = MemoryBudget::new(16 * 1024 * 1024);
    let original = uqa_core::CancellationToken::new();
    let invoking = uqa_core::CancellationToken::new();
    for token in [&original, &invoking] {
        token.cancel();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = engine
            .collect_hierarchy_analyze_statistics(
                "public.distribution",
                &["payload".into()],
                true,
                control,
            )
            .unwrap_err();
        assert_eq!(
            uqa_sql::catalog::errors::storage_error("test cancellation", &error).sqlstate(),
            Some("57014")
        );
        assert!(table.column_stats.read().is_empty());
        assert_eq!(budget.used(), 0);
        token.reset();
    }
    drop(table);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(retained.memory().used(), 0);
}
