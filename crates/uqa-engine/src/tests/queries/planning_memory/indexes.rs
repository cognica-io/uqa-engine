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
fn native_index_candidates_borrow_unselected_filter_payloads(#[case] provider: Provider) {
    use uqa_core::Predicate;
    use uqa_operators::OperatorTree;
    use uqa_planner::retrieval_planning::query_optimizer;

    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("candidate-inputs.db"));
    engine.sql("CREATE TABLE candidate_items(id integer PRIMARY KEY, payload text); INSERT INTO candidate_items VALUES (1, 'x')", &[]).unwrap();
    let indexed = OperatorTree::Filter {
        field: "id".into(),
        predicate: Predicate::Equals(Value::Int(1)),
        source: None,
    };
    let expected = query_optimizer(&engine, "candidate_items", &indexed).unwrap();
    assert_eq!(expected.index_candidates.len(), 1);
    let mut children: Vec<_> = (0..8192)
        .map(|_| OperatorTree::Filter {
            field: "payload".into(),
            predicate: Predicate::Equals(Value::Null),
            source: None,
        })
        .collect();
    children.push(OperatorTree::Filter {
        field: "payload".into(),
        predicate: Predicate::Equals(Value::Str("x".repeat(4 * 1024 * 1024))),
        source: None,
    });
    children.push(indexed);
    let tree = OperatorTree::Union(children);
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let before = memory.used();
    let sibling = memory.reserve(memory.available() - 32 * 1024).unwrap();
    let mut actual = None;
    let allocation = allocation_counter::measure(|| {
        actual = Some(query_optimizer(&engine, "candidate_items", &tree).unwrap());
    });
    assert!(
        allocation.bytes_total < 64 * 1024,
        "{provider:?}: {allocation:?}"
    );
    let actual = actual.unwrap();
    assert_eq!(
        actual.index_candidates, expected.index_candidates,
        "{provider:?}"
    );
    assert_eq!(
        actual.index_stats.total_docs,
        expected.index_stats.total_docs
    );
    assert_eq!(memory.used(), before + sibling.bytes());
    drop(actual);
    drop(sibling);
    assert_eq!(memory.used(), before);
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_planning_index_capture_reuses_catalog_payloads(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("index-capture.db"));
    engine
        .sql("CREATE TABLE capture_items(id integer); CREATE INDEX capture_items_id ON capture_items(id)", &[])
        .unwrap();
    let plan = UnifiedPlan::lower(
        uqa_sql::compile("SELECT id FROM capture_items WHERE id = $1")
            .unwrap()
            .remove(0),
    );
    let estimate = || crate::capabilities::statement_planning::estimate_engine_plan(&engine, &plan);
    let expected = estimate().unwrap();
    engine.synchronize_catalog_registries().unwrap();
    let identity = uqa_core::RelationIdentity::new("public", "capture_items_id");
    engine
        .durable
        .catalog_indexes
        .write()
        .get_mut(&identity)
        .unwrap()
        .parameters_json = "x".repeat(4 * 1024 * 1024);
    let retained = engine.durable.catalog_indexes.snapshot();
    let original = &retained[&identity];
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some((
            PlannerStatisticsCatalog::catalog_indexes(&engine).unwrap(),
            RetrievalPlanningCatalog::list_catalog_indexes(&engine).unwrap(),
        ));
    });
    assert!(
        allocation.bytes_total < 16 * 1024,
        "{provider:?}: {allocation:?}"
    );
    let output = output.unwrap();
    for rows in [&output.0, &output.1] {
        let captured = rows.iter().find(|row| row.relation == identity).unwrap();
        assert!(std::ptr::eq(captured, original), "{provider:?}");
        assert_eq!(
            captured.parameters_json.as_ptr(),
            original.parameters_json.as_ptr()
        );
    }
    let retained_control = engine.query_retention_control().unwrap();
    let memory = retained_control.memory();
    let before = memory.used();
    let sibling = memory.reserve(memory.available() - 16 * 1024).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| result = Some(estimate()));
    let actual = result.unwrap().unwrap();
    assert!(
        allocation.bytes_total < 64 * 1024,
        "{provider:?}: {allocation:?}"
    );
    assert_eq!(actual.rows.to_bits(), expected.rows.to_bits());
    assert_eq!(actual.execution.to_bits(), expected.execution.to_bits());
    assert_eq!(actual.startup.to_bits(), expected.startup.to_bits());
    assert_eq!(memory.used(), before + sibling.bytes());
    drop(sibling);
    assert_eq!(memory.used(), before);
    engine.close().unwrap();
    assert_eq!(
        output.0.iter().next().unwrap().parameters_json.len(),
        4 * 1024 * 1024
    );
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_planning_index_snapshots_follow_transactional_publication(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("index-publication.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE snapshot_items(id integer, value integer); CREATE INDEX original_index ON snapshot_items(id)", &[]).unwrap();
    let capture = || PlannerStatisticsCatalog::catalog_indexes(&engine).unwrap();
    let names = |rows: &uqa_core::catalog_index::CatalogIndexSnapshot| {
        rows.iter()
            .map(|row| row.relation.name.clone())
            .collect::<Vec<_>>()
    };
    let before = capture();
    assert_eq!(names(&before), ["original_index"]);
    engine
        .sql(
            "BEGIN; DROP INDEX original_index; CREATE INDEX private_index ON snapshot_items(value)",
            &[],
        )
        .unwrap();
    let private = capture();
    assert_eq!(names(&private), ["private_index"]);
    engine.sql("ROLLBACK", &[]).unwrap();
    let restored = capture();
    assert_eq!(names(&restored), ["original_index"]);
    engine
        .sql("CREATE INDEX committed_index ON snapshot_items(value)", &[])
        .unwrap();
    let committed = RetrievalPlanningCatalog::list_catalog_indexes(&engine).unwrap();
    assert_eq!(names(&committed), ["committed_index", "original_index"]);
    let mut public_copy = engine.list_catalog_indexes().unwrap();
    public_copy[0].parameters_json = "independent public output".into();
    assert_ne!(
        committed.iter().next().unwrap().parameters_json,
        public_copy[0].parameters_json
    );
    assert_eq!(names(&capture()), names(&committed));
    engine.close().unwrap();
    drop(engine);
    assert_eq!(names(&before), ["original_index"]);
    assert_eq!(names(&private), ["private_index"]);
    assert_eq!(names(&restored), ["original_index"]);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        let persisted = PlannerStatisticsCatalog::catalog_indexes(&reopened).unwrap();
        assert_eq!(names(&persisted), names(&committed));
        reopened.close().unwrap();
    }
}

#[test]
fn native_planning_index_capture_preserves_the_pinned_catalog() {
    let mut engine = Engine::new();
    engine
        .sql("CREATE TABLE pinned_items(id integer PRIMARY KEY)", &[])
        .unwrap();
    let original = PlannerStatisticsCatalog::catalog_indexes(&engine).unwrap();
    let retained = std::sync::Arc::new(crate::session::RetainedCatalogSnapshot {
        durable: engine.durable.snapshot(),
        read_view: engine.catalog_read_view(),
    });
    engine
        .sql(
            "ALTER TABLE pinned_items DROP CONSTRAINT pinned_items_pkey",
            &[],
        )
        .unwrap();
    assert!(PlannerStatisticsCatalog::catalog_indexes(&engine)
        .unwrap()
        .is_empty());
    engine.query_catalog_snapshot = Some(retained);
    let allocation = allocation_counter::measure(|| {
        let statement = PlannerStatisticsCatalog::catalog_indexes(&engine).unwrap();
        let retrieval = RetrievalPlanningCatalog::list_catalog_indexes(&engine).unwrap();
        for snapshot in [statement, retrieval] {
            assert_eq!(snapshot.len(), 1);
            assert!(std::ptr::eq(
                snapshot.iter().next().unwrap(),
                original.iter().next().unwrap()
            ));
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    engine.close().unwrap();
}
