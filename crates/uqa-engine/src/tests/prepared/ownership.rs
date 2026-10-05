//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::sync::Arc;
use uqa_planner::statement_planning::prepared::selection::PreparedPlanSession;
use uqa_sql::prepared::planning::PreparedPlanUpdate;

fn register(engine: &Engine, sql: &str) -> Result<(), uqa_sql::SQLError> {
    engine.register_prepared("saved".into(), uqa_sql::compile(sql).unwrap().remove(0))
}

#[test]
fn replaced_or_deallocated_definition_never_receives_a_previous_plans_usage() {
    let engine = Engine::new();
    register(&engine, "SELECT 1").unwrap();
    let original = engine.session.prepared.read()["saved"].logical_plan.clone();
    register(&engine, "SELECT 2").unwrap();
    let update = || PreparedPlanUpdate {
        reanalyzed: None,
        generic_plan: Some((*original).clone()),
        generic_cost: Some(42.0),
        custom_cost: None,
    };
    PreparedPlanSession::publish_usage(&engine, "saved", &original, update());
    let current = engine.session.prepared.read()["saved"].clone();
    assert!(!Arc::ptr_eq(&current.logical_plan, &original));
    assert!(current.plan.is_none());
    assert!(current.generic_cost.is_none());
    assert_eq!(current.generic_plans, 0);
    assert_eq!(current.custom_plans, 0);
    engine.deallocate_prepared(Some("saved"));
    PreparedPlanSession::publish_usage(&engine, "saved", &original, update());
    assert!(!engine.session.prepared.read().contains_key("saved"));
}

#[test]
fn failed_replacement_keeps_the_original_definition_identity_and_metadata() {
    let engine = Engine::new();
    register(&engine, "SELECT 1 AS value").unwrap();
    let original = engine.session.prepared.read()["saved"].clone();
    assert!(register(&engine, "SELECT missing FROM missing_table").is_err());
    let current = engine.session.prepared.read()["saved"].clone();
    assert!(Arc::ptr_eq(&current.logical_plan, &original.logical_plan));
    assert_eq!(current.prepared_at_micros, original.prepared_at_micros);
    assert_eq!(current.parameter_types, original.parameter_types);
    assert_eq!(current.generic_plans, original.generic_plans);
    assert_eq!(current.custom_plans, original.custom_plans);
}

#[rstest::rstest]
#[case::native_sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn direct_registration_retains_one_fresh_peer_catalog_boundary(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("direct-prepared.db");
    let engine = match provider {
        0 => Engine::open(&path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    engine
        .sql("CREATE TABLE direct_prepared_rows (id integer)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO direct_prepared_rows VALUES (1)", &[])
        .unwrap();
    let peer = engine.new_session().unwrap();
    peer.sql(
        "ALTER TABLE direct_prepared_rows ADD COLUMN added integer DEFAULT 7",
        &[],
    )
    .unwrap();

    register(&engine, "SELECT added FROM direct_prepared_rows").unwrap();
    let original = engine.session.prepared.read()["saved"].clone();
    assert!(!original.dependencies.relations.is_empty());
    assert!(original.dependency_snapshot.is_some());
    let result = engine.sql("EXECUTE saved", &[]).unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["added"], uqa_core::Value::Int(7));
    let current = engine.session.prepared.read()["saved"].clone();
    assert!(Arc::ptr_eq(&original.logical_plan, &current.logical_plan));
    assert_eq!(original.dependency_snapshot, current.dependency_snapshot);
}

#[rstest::rstest]
#[case::drop_column("ALTER TABLE prepared_relation DROP COLUMN extra")]
#[case::rename_column("ALTER TABLE prepared_relation RENAME COLUMN extra TO renamed")]
#[case::rename_table_roundtrip("ALTER TABLE prepared_relation RENAME TO renamed_relation; ALTER TABLE renamed_relation RENAME TO prepared_relation")]
fn memory_relation_publications_reread_prepared_inputs(#[case] change: &str) {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE prepared_relation (id integer, extra integer)",
            &[],
        )
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine.session.transactions.lock()[0].started_at_micros = 90_123_456_789;
    engine.sql("PREPARE saved AS SELECT 'now'::timestamp AS stamp, (SELECT count(*) FROM prepared_relation) AS rows", &[]).unwrap();
    let original = engine.session.prepared.read()["saved"].logical_plan.clone();
    engine.sql("COMMIT", &[]).unwrap();
    assert!(!engine.session.prepared.read()["saved"].needs_analysis);

    engine.sql(change, &[]).unwrap();
    assert!(
        engine.session.prepared.read()["saved"].needs_analysis,
        "{change}"
    );
    engine.sql("BEGIN", &[]).unwrap();
    let current_clock = 190_123_456_789;
    engine.session.transactions.lock()[0].started_at_micros = current_clock;
    let result = engine.sql("EXECUTE saved", &[]).unwrap();
    assert_eq!(
        result.rows[0]["stamp"],
        uqa_core::Value::Temporal(uqa_core::TemporalValue::Timestamp {
            micros: current_clock
        })
    );
    assert!(!Arc::ptr_eq(
        &original,
        &engine.session.prepared.read()["saved"].logical_plan
    ));
    engine.sql("COMMIT", &[]).unwrap();
}
