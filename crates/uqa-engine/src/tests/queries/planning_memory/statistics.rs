//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_planner::retrieval_planning::RetrievalPlanningCatalog;
use uqa_planner::statement_planning::PlannerStatisticsCatalog;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_statistics_capture_reuses_catalog_payloads(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("statistics-owner.db"));
    engine.sql("CREATE TABLE stats_capture (payload text); INSERT INTO stats_capture VALUES ('x'), ('y'); ANALYZE stats_capture", &[]).unwrap();
    let table = engine.try_query_table("stats_capture").unwrap().unwrap();
    table.column_stats.write().insert(
        "payload".into(),
        uqa_planner::ColumnStats {
            row_count: 2,
            distinct_count: 2,
            mcv_values: vec![Value::Str("x".repeat(4 * 1024 * 1024))],
            mcv_frequencies: vec![0.5],
            ..Default::default()
        },
    );
    let retained = table.column_stats.snapshot();
    let pointer = retained["payload"].mcv_values.as_ptr();
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some((
            PlannerStatisticsCatalog::column_statistics(&engine, "stats_capture").unwrap(),
            RetrievalPlanningCatalog::try_query_column_stats(&engine, "stats_capture").unwrap(),
        ));
    });
    assert!(
        allocation.bytes_total < 16 * 1024,
        "{provider:?}: {allocation:?}"
    );
    let output = output.unwrap();
    assert_eq!(output.0["payload"].mcv_values.as_ptr(), pointer);
    assert_eq!(output.1["payload"].mcv_values.as_ptr(), pointer);
    drop(output);
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_statistics_snapshots_follow_transactional_publication(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("statistics-publication.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE stats_publication (id integer); INSERT INTO stats_publication VALUES (1), (2), (3); ANALYZE stats_publication", &[]).unwrap();
    let capture =
        || PlannerStatisticsCatalog::column_statistics(&engine, "stats_publication").unwrap();
    let before = capture();
    assert_eq!(before["id"].row_count, 3);
    assert_eq!(before["id"].max_value, Some(Value::Int(3)));
    engine
        .sql(
            "BEGIN; INSERT INTO stats_publication VALUES (4), (5), (6); ANALYZE stats_publication",
            &[],
        )
        .unwrap();
    let private = capture();
    assert_eq!(private["id"].row_count, 6);
    assert_eq!(private["id"].max_value, Some(Value::Int(6)));
    assert!(!std::ptr::eq(&raw const *before, &raw const *private));
    engine.sql("ROLLBACK", &[]).unwrap();
    let restored = capture();
    assert_eq!(restored["id"].row_count, 3);
    assert_eq!(restored["id"].max_value, Some(Value::Int(3)));
    engine
        .sql(
            "INSERT INTO stats_publication VALUES (7); ANALYZE stats_publication",
            &[],
        )
        .unwrap();
    let committed = capture();
    assert_eq!(committed["id"].row_count, 4);
    assert_eq!(committed["id"].max_value, Some(Value::Int(7)));
    engine.close().unwrap();
    drop(engine);
    for columns in [&before, &restored] {
        assert_eq!(columns["id"].row_count, 3);
        assert_eq!(columns["id"].max_value, Some(Value::Int(3)));
    }
    assert_eq!(private["id"].row_count, 6);
    assert_eq!(private["id"].max_value, Some(Value::Int(6)));
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        let persisted =
            PlannerStatisticsCatalog::column_statistics(&reopened, "stats_publication").unwrap();
        assert_eq!(persisted["id"].row_count, 4);
        assert_eq!(persisted["id"].max_value, Some(Value::Int(7)));
        assert_eq!(
            reopened
                .sql("SELECT count(*) AS n FROM stats_publication", &[])
                .unwrap()
                .rows,
            vec![std::collections::BTreeMap::from([(
                "n".into(),
                Value::Int(4)
            )])]
        );
        reopened.close().unwrap();
    }
}
