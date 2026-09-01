//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::StoredViewKind;

#[test]
fn capability_views_expose_only_their_owned_state() {
    let engine = Engine::new();
    let catalog = engine.catalog_read_view();
    let session = engine.session_execution_view();
    let runtime = engine.query_runtime_view();
    assert!(catalog.has_schema("public"));
    assert_eq!(session.search_path(), vec!["public"]);
    assert_eq!(session.current_user(), "uqa");
    assert_eq!(session.session_user(), "uqa");
    assert_eq!(session.transaction_depth(), 0);
    assert_eq!(session.transaction_snapshot_identity(), None);
    assert_eq!(runtime.work_mem_bytes().unwrap(), 64 * 1024 * 1024);
    runtime.check_cancelled().unwrap();
}

#[test]
fn mutation_coordinator_publishes_schema_changes_without_engine_recovery() {
    let engine = Engine::new();
    let snapshot = engine.catalog_read_view();
    let before = engine.catalog_epochs();
    assert!(engine
        .mutation_coordinator()
        .register_schema("capability_test", false)
        .unwrap());
    assert!(!snapshot.has_schema("capability_test"));
    assert!(engine.catalog_read_view().has_schema("capability_test"));
    let after = engine.catalog_epochs();
    assert_eq!(after.catalog_registry, before.catalog_registry);
    assert!(
        engine
            .epochs
            .catalog_registry
            .published
            .load(Ordering::Acquire)
            > before.catalog_registry
    );
}

#[test]
fn catalog_read_view_keeps_statement_relation_snapshot() {
    let engine = Engine::new();
    let snapshot = engine.catalog_read_view();
    engine
        .create_table(
            "snapshot_table",
            uqa_analysis::standard_analyzer("english"),
            Vec::new(),
        )
        .unwrap();
    let resolution = engine.session_execution_view().relation_name_resolution();
    assert_eq!(
        snapshot.table_name(&resolution, "snapshot_table").unwrap(),
        None
    );
    assert_eq!(
        engine
            .catalog_read_view()
            .table_name(&resolution, "snapshot_table")
            .unwrap()
            .as_deref(),
        Some("public.snapshot_table")
    );
}

#[test]
fn catalog_read_view_keeps_all_live_projection_families_on_one_snapshot() {
    let engine = Engine::new();
    let snapshot = engine.catalog_read_view();

    engine
        .sql("CREATE SEQUENCE snapshot_sequence", &[])
        .unwrap();
    engine
        .sql("CREATE VIEW snapshot_view AS SELECT 1 AS id", &[])
        .unwrap();
    engine.sql("CREATE ROLE snapshot_role", &[]).unwrap();
    engine.create_graph("snapshot_graph").unwrap();

    assert!(snapshot.sequences().is_empty());
    assert!(snapshot.views_of_kind(StoredViewKind::View).is_empty());
    assert!(!snapshot.roles().any(|role| role.name == "snapshot_role"));
    assert!(snapshot.graph_names().is_empty());

    let current = engine.catalog_read_view();
    assert!(current
        .sequences()
        .iter()
        .any(|(name, _, _)| name == "public.snapshot_sequence"));
    assert!(current
        .views_of_kind(StoredViewKind::View)
        .iter()
        .any(|(name, _)| name == "public.snapshot_view"));
    assert!(current.roles().any(|role| role.name == "snapshot_role"));
    assert_eq!(current.graph_names(), vec!["snapshot_graph"]);
}

#[test]
fn catalog_read_view_keeps_routines_triggers_and_rules_on_one_snapshot() {
    let engine = Engine::new();
    let snapshot = engine.catalog_read_view();

    engine
        .sql(
            "CREATE TABLE snapshot_events (id INTEGER); CREATE FUNCTION snapshot_trigger_function() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END'; CREATE TRIGGER snapshot_trigger BEFORE INSERT ON snapshot_events FOR EACH ROW EXECUTE FUNCTION snapshot_trigger_function(); CREATE RULE snapshot_rule AS ON UPDATE TO snapshot_events DO ALSO NOTHING",
            &[],
        )
        .unwrap();

    assert!(snapshot.all_sql_functions().is_empty());
    assert!(snapshot.triggers().is_empty());
    assert!(snapshot.rules().is_empty());

    let current = engine.catalog_read_view();
    assert_eq!(current.all_sql_functions().len(), 1);
    assert!(current
        .triggers()
        .iter()
        .any(|trigger| trigger.definition.name == "snapshot_trigger"));
    assert!(current
        .rules()
        .iter()
        .any(|rule| rule.definition.name == "snapshot_rule"));
}

#[test]
fn relation_name_resolution_keeps_the_statement_search_path() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SCHEMA first_path; CREATE SCHEMA second_path; CREATE TABLE first_path.items (id integer); CREATE TABLE second_path.items (id integer)",
            &[],
        )
        .unwrap();
    engine.set_variable("search_path", "first_path").unwrap();
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();

    engine.set_variable("search_path", "second_path").unwrap();
    let current_resolution = engine.session_execution_view().relation_name_resolution();

    assert_eq!(
        catalog.table_name(&resolution, "items").unwrap().as_deref(),
        Some("first_path.items")
    );
    assert_eq!(
        catalog
            .table_name(&current_resolution, "items")
            .unwrap()
            .as_deref(),
        Some("second_path.items")
    );
}
