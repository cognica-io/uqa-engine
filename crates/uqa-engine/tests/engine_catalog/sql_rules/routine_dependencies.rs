//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn bound_rule_condition_routines_enforce_exact_drop_dependencies() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rule-routine-dependency.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE SCHEMA rule_routine_dependency;
             CREATE FUNCTION rule_routine_dependency.accepted(value INTEGER) RETURNS BOOLEAN
               LANGUAGE SQL IMMUTABLE AS 'SELECT true';
             CREATE FUNCTION rule_routine_dependency.accepted(value TEXT) RETURNS BOOLEAN
               LANGUAGE SQL IMMUTABLE AS 'SELECT false';
             CREATE TABLE rule_routine_dependency.events(id INTEGER);
             CREATE RULE condition_routine_dependency AS ON INSERT TO rule_routine_dependency.events
               WHERE EXISTS (SELECT 1 WHERE rule_routine_dependency.accepted(NEW.id))
               DO NOTHING",
        );
    }
    let engine = Engine::open(&path).expect("rule routine dependencies must restore");
    exec(
        &engine,
        "DROP FUNCTION rule_routine_dependency.accepted(TEXT)",
    );
    let error = engine
        .sql(
            "DROP FUNCTION rule_routine_dependency.accepted(INTEGER)",
            &[],
        )
        .expect_err("the exact condition overload must be protected by RESTRICT");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    exec(
        &engine,
        "DROP FUNCTION rule_routine_dependency.accepted(INTEGER) CASCADE",
    );
    assert!(exec(
        &engine,
        "SELECT rulename FROM pg_rewrite WHERE rulename = 'condition_routine_dependency'",
    )
    .rows
    .is_empty());
}

#[test]
fn bound_rule_action_routines_enforce_exact_drop_dependencies() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE SCHEMA rule_action_routine;
         CREATE FUNCTION rule_action_routine.mapped(value INTEGER) RETURNS INTEGER
           LANGUAGE SQL IMMUTABLE AS 'SELECT $1 + 10';
         CREATE FUNCTION rule_action_routine.mapped(value TEXT) RETURNS TEXT
           LANGUAGE SQL IMMUTABLE AS 'SELECT $1 || ''x''';
         CREATE TABLE rule_action_routine.events(id INTEGER);
         CREATE TABLE rule_action_routine.log(value INTEGER);
         CREATE RULE action_routine_dependency AS ON INSERT TO rule_action_routine.events DO ALSO
           INSERT INTO rule_action_routine.log VALUES (rule_action_routine.mapped(NEW.id))",
    );
    exec(&engine, "DROP FUNCTION rule_action_routine.mapped(TEXT)");
    let error = engine
        .sql("DROP FUNCTION rule_action_routine.mapped(INTEGER)", &[])
        .expect_err("the exact action overload must be protected by RESTRICT");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    exec(
        &engine,
        "DROP FUNCTION rule_action_routine.mapped(INTEGER) CASCADE",
    );
    assert!(exec(
        &engine,
        "SELECT rulename FROM pg_rewrite WHERE rulename = 'action_routine_dependency'",
    )
    .rows
    .is_empty());
}

#[test]
fn rule_routine_dependency_multi_drop_is_atomic() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE SCHEMA routine_drop_atomicity;
         CREATE FUNCTION routine_drop_atomicity.mapped(value INTEGER) RETURNS INTEGER
           LANGUAGE SQL IMMUTABLE AS 'SELECT $1 + 10';
         CREATE FUNCTION routine_drop_atomicity.unrelated(value INTEGER) RETURNS INTEGER
           LANGUAGE SQL IMMUTABLE AS 'SELECT $1 + 100';
         CREATE TABLE routine_drop_atomicity.events(id INTEGER);
         CREATE TABLE routine_drop_atomicity.log(value INTEGER);
         CREATE RULE routine_drop_atomicity_rule AS ON INSERT TO routine_drop_atomicity.events DO ALSO
           INSERT INTO routine_drop_atomicity.log VALUES (routine_drop_atomicity.mapped(NEW.id))",
    );

    let error = engine
        .sql(
            "DROP FUNCTION routine_drop_atomicity.unrelated(INTEGER),
                           routine_drop_atomicity.mapped(INTEGER)",
            &[],
        )
        .expect_err("a dependent overload must reject the complete multi-function drop");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
    assert_eq!(
        exec(
            &engine,
            "SELECT routine_drop_atomicity.unrelated(1) AS value",
        )
        .rows[0]
            .get("value"),
        Some(&Value::Int(101))
    );
    exec(
        &engine,
        "INSERT INTO routine_drop_atomicity.events VALUES (1)",
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM routine_drop_atomicity.log").rows[0].get("value"),
        Some(&Value::Int(11))
    );
}

#[test]
fn function_cascade_drops_rules_that_depend_on_cascading_views() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE FUNCTION cascading_rule_view_value(value INTEGER) RETURNS INTEGER
           LANGUAGE SQL IMMUTABLE AS 'SELECT $1 + 10';
         CREATE TABLE cascading_rule_view_source(id INTEGER);
         CREATE VIEW cascading_rule_view AS
           SELECT cascading_rule_view_value(id) AS value FROM cascading_rule_view_source;
         CREATE TABLE cascading_rule_view_events(id INTEGER);
         CREATE TABLE cascading_rule_view_log(value INTEGER);
         CREATE RULE cascading_rule_view_dependency AS ON INSERT TO cascading_rule_view_events DO ALSO
           INSERT INTO cascading_rule_view_log SELECT value FROM cascading_rule_view;
         DROP FUNCTION cascading_rule_view_value(INTEGER) CASCADE",
    );
    assert!(exec(
        &engine,
        "SELECT rulename FROM pg_rewrite
             WHERE rulename = 'cascading_rule_view_dependency'",
    )
    .rows
    .is_empty());
    assert!(exec(
        &engine,
        "SELECT relname FROM pg_class WHERE relname = 'cascading_rule_view'",
    )
    .rows
    .is_empty());
}

#[test]
fn rule_action_routines_are_creation_bound_across_search_path_and_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rule-action-routine-binding.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE SCHEMA action_routine_first;
             CREATE SCHEMA action_routine_second;
             CREATE FUNCTION action_routine_first.mapped(value INTEGER) RETURNS INTEGER
               LANGUAGE SQL IMMUTABLE AS 'SELECT $1 + 10';
             CREATE FUNCTION action_routine_second.mapped(value INTEGER) RETURNS INTEGER
               LANGUAGE SQL IMMUTABLE AS 'SELECT $1 + 100';
             CREATE TABLE action_routine_first.events(id INTEGER);
             CREATE TABLE action_routine_first.log(value INTEGER);
             SET search_path = action_routine_first, public;
             CREATE RULE bound_action_routine AS ON INSERT TO events DO ALSO
               INSERT INTO log VALUES (mapped(NEW.id));
             SET search_path = action_routine_second, public;
             INSERT INTO action_routine_first.events VALUES (1)",
        );
        assert_eq!(
            exec(&engine, "SELECT value FROM action_routine_first.log").rows[0].get("value"),
            Some(&Value::Int(11))
        );
    }
    let engine = Engine::open(&path).expect("bound rule action routine must restore");
    exec(
        &engine,
        "SET search_path = action_routine_second, public;
         INSERT INTO action_routine_first.events VALUES (2)",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT value FROM action_routine_first.log ORDER BY value",
        )
        .rows
        .iter()
        .map(|row| row.get("value"))
        .collect::<Vec<_>>(),
        [Some(&Value::Int(11)), Some(&Value::Int(12))]
    );
}

#[test]
fn scalar_rule_condition_routines_are_creation_bound_and_drop_dependent() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rule-scalar-condition-routine.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE SCHEMA scalar_condition_first;
             CREATE SCHEMA scalar_condition_second;
             CREATE FUNCTION scalar_condition_first.accepted(value INTEGER) RETURNS BOOLEAN
               LANGUAGE SQL IMMUTABLE AS 'SELECT true';
             CREATE FUNCTION scalar_condition_second.accepted(value INTEGER) RETURNS BOOLEAN
               LANGUAGE SQL IMMUTABLE AS 'SELECT false';
             CREATE TABLE scalar_condition_first.events(id INTEGER);
             SET search_path = scalar_condition_first, public;
             CREATE RULE bound_scalar_condition AS ON INSERT TO events
               WHERE accepted(NEW.id) DO INSTEAD NOTHING;
             SET search_path = scalar_condition_second, public;
             INSERT INTO scalar_condition_first.events VALUES (1)",
        );
        assert!(
            exec(&engine, "SELECT id FROM scalar_condition_first.events")
                .rows
                .is_empty()
        );
    }
    let engine = Engine::open(&path).expect("bound scalar rule condition must restore");
    exec(
        &engine,
        "SET search_path = scalar_condition_second, public;
         INSERT INTO scalar_condition_first.events VALUES (2)",
    );
    assert!(
        exec(&engine, "SELECT id FROM scalar_condition_first.events")
            .rows
            .is_empty()
    );
    let error = engine
        .sql(
            "DROP FUNCTION scalar_condition_first.accepted(INTEGER)",
            &[],
        )
        .expect_err("the scalar condition routine must remain dependent");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
}

#[test]
fn each_rule_action_call_keeps_its_exact_overload_after_catalog_changes() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rule-action-call-bindings.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE SCHEMA action_call_binding;
             CREATE FUNCTION action_call_binding.mapped(value BIGINT) RETURNS INTEGER
               LANGUAGE SQL IMMUTABLE AS 'SELECT 10';
             CREATE FUNCTION action_call_binding.mapped(value TEXT) RETURNS INTEGER
               LANGUAGE SQL IMMUTABLE AS 'SELECT 20';
             CREATE TABLE action_call_binding.events(id INTEGER, note TEXT);
             CREATE TABLE action_call_binding.log(first_value INTEGER, second_value INTEGER);
             CREATE RULE bound_action_calls AS ON INSERT TO action_call_binding.events DO ALSO
               INSERT INTO action_call_binding.log VALUES (
                 action_call_binding.mapped(NEW.id),
                 action_call_binding.mapped(NEW.note)
               );
             CREATE FUNCTION action_call_binding.mapped(value INTEGER) RETURNS INTEGER
               LANGUAGE SQL IMMUTABLE AS 'SELECT 99';
             INSERT INTO action_call_binding.events VALUES (1, 'one')",
        );
        let rows = exec(
            &engine,
            "SELECT first_value, second_value FROM action_call_binding.log",
        );
        assert_eq!(rows.rows[0].get("first_value"), Some(&Value::Int(10)));
        assert_eq!(rows.rows[0].get("second_value"), Some(&Value::Int(20)));
    }
    let engine = Engine::open(&path).expect("per-call rule overload bindings must restore");
    exec(
        &engine,
        "INSERT INTO action_call_binding.events VALUES (2, 'two')",
    );
    let rows = exec(
        &engine,
        "SELECT first_value, second_value FROM action_call_binding.log ORDER BY first_value, second_value",
    );
    assert_eq!(rows.rows.len(), 2);
    assert!(rows.rows.iter().all(|row| {
        row.get("first_value") == Some(&Value::Int(10))
            && row.get("second_value") == Some(&Value::Int(20))
    }));
}

#[test]
fn rule_action_table_functions_keep_exact_bindings_across_search_path_and_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rule-action-table-function.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE SCHEMA action_rows_first;
             CREATE SCHEMA action_rows_second;
             CREATE FUNCTION action_rows_first.chosen(value INTEGER) RETURNS TABLE(result INTEGER)
               LANGUAGE SQL IMMUTABLE AS 'SELECT $1 + 10';
             CREATE FUNCTION action_rows_second.chosen(value INTEGER) RETURNS TABLE(result INTEGER)
               LANGUAGE SQL IMMUTABLE AS 'SELECT $1 + 100';
             CREATE TABLE action_rows_first.events(id INTEGER);
             CREATE TABLE action_rows_first.log(value INTEGER);
             SET search_path = action_rows_first, public;
             CREATE RULE bound_action_rows AS ON INSERT TO events DO ALSO
               INSERT INTO log SELECT result FROM chosen(NEW.id);
             SET search_path = action_rows_second, public;
             INSERT INTO action_rows_first.events VALUES (1)",
        );
        assert_eq!(
            exec(&engine, "SELECT value FROM action_rows_first.log").rows[0].get("value"),
            Some(&Value::Int(11))
        );
    }
    let engine = Engine::open(&path).expect("bound rule table function must restore");
    exec(
        &engine,
        "SET search_path = action_rows_second, public;
         INSERT INTO action_rows_first.events VALUES (2)",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT value FROM action_rows_first.log ORDER BY value"
        )
        .rows
        .iter()
        .map(|row| row.get("value"))
        .collect::<Vec<_>>(),
        [Some(&Value::Int(11)), Some(&Value::Int(12))]
    );
    let error = engine
        .sql("DROP FUNCTION action_rows_first.chosen(INTEGER)", &[])
        .expect_err("the exact table-function dependency must be protected");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
}
