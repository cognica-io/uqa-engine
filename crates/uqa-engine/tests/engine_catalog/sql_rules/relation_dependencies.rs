//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn rule_action_sources_are_creation_bound_across_search_path_and_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rule-action-source.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE SCHEMA rule_source_first;
             CREATE SCHEMA rule_source_second;
             CREATE TABLE rule_source_first.lookup(id INTEGER);
             CREATE TABLE rule_source_second.lookup(id INTEGER);
             CREATE TABLE rule_source_first.events(id INTEGER);
             CREATE TABLE rule_source_first.log(id INTEGER);
             INSERT INTO rule_source_first.lookup VALUES (1);
             INSERT INTO rule_source_second.lookup VALUES (2);
             SET search_path = rule_source_first, public;
             CREATE RULE bound_action_source AS ON INSERT TO events DO ALSO
               INSERT INTO log SELECT id FROM lookup;
             SET search_path = rule_source_second, public;
             INSERT INTO rule_source_first.events VALUES (10)",
        );
        assert_eq!(
            exec(&engine, "SELECT id FROM rule_source_first.log").rows[0].get("id"),
            Some(&Value::Int(1))
        );
    }
    let engine = Engine::open(&path).expect("bound rule action source must restore");
    exec(
        &engine,
        "SET search_path = rule_source_second, public;
         INSERT INTO rule_source_first.events VALUES (20)",
    );
    assert_eq!(
        exec(&engine, "SELECT id FROM rule_source_first.log ORDER BY id",)
            .rows
            .iter()
            .map(|row| row.get("id"))
            .collect::<Vec<_>>(),
        [Some(&Value::Int(1)), Some(&Value::Int(1))]
    );
}

#[test]
fn rule_relation_dependencies_enforce_restrict_and_cascade() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE dependency_events(id INTEGER);
         CREATE TABLE condition_source(id INTEGER);
         CREATE TABLE action_source(id INTEGER);
         CREATE TABLE action_target(id INTEGER);
         CREATE RULE condition_dependency AS ON INSERT TO dependency_events
           WHERE EXISTS (SELECT 1 FROM condition_source WHERE id = NEW.id)
           DO NOTHING;
         CREATE RULE action_source_dependency AS ON INSERT TO dependency_events DO ALSO
           INSERT INTO action_target SELECT id FROM action_source",
    );

    for table in ["condition_source", "action_source", "action_target"] {
        let error = engine
            .sql(&format!("DROP TABLE {table}"), &[])
            .expect_err("a referenced relation must be protected by RESTRICT");
        assert_eq!(error.sqlstate(), Some("2BP01"), "{table}: {error}");
    }
    exec(
        &engine,
        "DROP TABLE condition_source, action_source CASCADE",
    );
    assert!(exec(
        &engine,
        "SELECT rulename FROM pg_rewrite WHERE rulename IN ('condition_dependency', 'action_source_dependency')",
    )
    .rows
    .is_empty());
    exec(&engine, "DROP TABLE action_target");
}

#[test]
fn rule_relation_dependency_drops_are_atomic_and_transactional() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE dependency_tx_events(id INTEGER);
         CREATE TABLE dependency_tx_source(id INTEGER);
         CREATE TABLE dependency_tx_unrelated(id INTEGER);
         CREATE TABLE dependency_tx_log(id INTEGER);
         INSERT INTO dependency_tx_source VALUES (7);
         CREATE RULE dependency_tx_rule AS ON INSERT TO dependency_tx_events DO ALSO
           INSERT INTO dependency_tx_log SELECT id FROM dependency_tx_source",
    );

    let error = engine
        .sql(
            "DROP TABLE dependency_tx_unrelated, dependency_tx_source",
            &[],
        )
        .expect_err("a multi-target RESTRICT failure must reject the complete drop");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS count FROM pg_class
             WHERE relname IN ('dependency_tx_source', 'dependency_tx_unrelated')",
        )
        .rows[0]
            .get("count"),
        Some(&Value::Int(2))
    );

    exec(&engine, "BEGIN");
    exec(&engine, "DROP TABLE dependency_tx_source CASCADE");
    assert!(exec(
        &engine,
        "SELECT rulename FROM pg_rewrite WHERE rulename = 'dependency_tx_rule'",
    )
    .rows
    .is_empty());
    exec(&engine, "ROLLBACK");
    exec(&engine, "INSERT INTO dependency_tx_events VALUES (1)");
    assert_eq!(
        exec(&engine, "SELECT id FROM dependency_tx_log").rows[0].get("id"),
        Some(&Value::Int(7))
    );
}

#[test]
fn rule_relation_dependencies_follow_table_rename_and_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rule-relation-rename.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE rename_dependency_events(id INTEGER);
             CREATE TABLE rename_dependency_source(id INTEGER);
             CREATE TABLE rename_dependency_log(id INTEGER);
             INSERT INTO rename_dependency_source VALUES (1);
             CREATE RULE rename_dependency_rule AS ON INSERT TO rename_dependency_events
               WHERE EXISTS (SELECT 1 FROM rename_dependency_source WHERE id = NEW.id)
               DO ALSO INSERT INTO rename_dependency_log
                 SELECT id FROM rename_dependency_source;
             ALTER TABLE rename_dependency_source RENAME TO renamed_dependency_source;
             ALTER TABLE rename_dependency_log RENAME TO renamed_dependency_log;
             INSERT INTO rename_dependency_events VALUES (1), (2)",
        );
        assert_eq!(
            exec(&engine, "SELECT id FROM renamed_dependency_log").rows[0].get("id"),
            Some(&Value::Int(1))
        );
    }
    let engine = Engine::open(&path).expect("renamed rule dependencies must restore");
    exec(
        &engine,
        "INSERT INTO rename_dependency_events VALUES (1), (2)",
    );
    assert_eq!(
        exec(&engine, "SELECT id FROM renamed_dependency_log ORDER BY id")
            .rows
            .iter()
            .map(|row| row.get("id"))
            .collect::<Vec<_>>(),
        [Some(&Value::Int(1)), Some(&Value::Int(1))]
    );
    let error = engine
        .sql("DROP TABLE renamed_dependency_source", &[])
        .expect_err("the renamed source must remain a rule dependency");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    let error = engine
        .sql("DROP TABLE renamed_dependency_log", &[])
        .expect_err("the renamed target must remain a rule dependency");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
}

#[test]
fn rule_relation_dependencies_cover_sequences_and_cascading_views() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE SEQUENCE dependency_sequence;
         CREATE TABLE sequence_dependency_events(id INTEGER);
         CREATE TABLE sequence_dependency_log(value BIGINT);
         CREATE RULE sequence_dependency_rule AS ON INSERT TO sequence_dependency_events DO ALSO
           INSERT INTO sequence_dependency_log SELECT last_value FROM dependency_sequence;
         ALTER SEQUENCE dependency_sequence RENAME TO renamed_dependency_sequence;
         INSERT INTO sequence_dependency_events VALUES (1)",
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM sequence_dependency_log").rows[0].get("value"),
        Some(&Value::Int(1))
    );
    let error = engine
        .sql("DROP SEQUENCE renamed_dependency_sequence", &[])
        .expect_err("a sequence row source must be protected by RESTRICT");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    exec(&engine, "DROP SEQUENCE renamed_dependency_sequence CASCADE");
    assert!(exec(
        &engine,
        "SELECT rulename FROM pg_rewrite WHERE rulename = 'sequence_dependency_rule'",
    )
    .rows
    .is_empty());

    exec(
        &engine,
        "CREATE TABLE cascading_view_base(id INTEGER);
         CREATE VIEW cascading_rule_source AS SELECT id FROM cascading_view_base;
         CREATE TABLE cascading_view_events(id INTEGER);
         CREATE TABLE cascading_view_log(id INTEGER);
         CREATE RULE cascading_view_rule AS ON INSERT TO cascading_view_events DO ALSO
           INSERT INTO cascading_view_log SELECT id FROM cascading_rule_source",
    );
    let error = engine
        .sql("DROP VIEW cascading_rule_source", &[])
        .expect_err("a rule source view must be protected by RESTRICT");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    exec(&engine, "DROP VIEW cascading_rule_source CASCADE");
    assert!(exec(
        &engine,
        "SELECT rulename FROM pg_rewrite WHERE rulename = 'cascading_view_rule'",
    )
    .rows
    .is_empty());
}

#[test]
fn rule_condition_dependencies_respect_sequential_cte_scope() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE cte_dependency_source(id INTEGER);
         CREATE TABLE cte_dependency_events(id INTEGER);
         CREATE RULE cte_dependency_rule AS ON INSERT TO cte_dependency_events
           WHERE EXISTS (
             WITH first_cte AS (SELECT id FROM cte_dependency_source),
                  cte_dependency_source AS (SELECT 999 AS id)
             SELECT 1 FROM first_cte WHERE id = NEW.id
           ) DO INSTEAD NOTHING",
    );
    let error = engine
        .sql("DROP TABLE cte_dependency_source", &[])
        .expect_err("a later CTE must not shadow a relation in an earlier CTE body");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    exec(&engine, "DROP TABLE cte_dependency_source CASCADE");
    assert!(exec(
        &engine,
        "SELECT rulename FROM pg_rewrite WHERE rulename = 'cte_dependency_rule'",
    )
    .rows
    .is_empty());
}

#[test]
fn rule_action_ctes_do_not_create_false_relation_dependencies() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE action_cte_name(id INTEGER);
         CREATE TABLE action_cte_events(id INTEGER);
         CREATE TABLE action_cte_log(id INTEGER);
         CREATE RULE action_cte_rule AS ON INSERT TO action_cte_events DO ALSO
           WITH action_cte_name AS (SELECT 7 AS id)
           INSERT INTO action_cte_log SELECT id FROM action_cte_name;
         DROP TABLE action_cte_name;
         INSERT INTO action_cte_events VALUES (7)",
    );
    assert_eq!(
        exec(&engine, "SELECT id FROM action_cte_log").rows[0].get("id"),
        Some(&Value::Int(7))
    );
}
