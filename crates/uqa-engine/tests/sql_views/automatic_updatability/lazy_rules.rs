//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.4 RULE projection barriers, checked against the Docker reference.

use super::*;

#[test]
fn referenced_volatile_view_outputs_materialize_once_even_in_a_skipped_case() {
    for condition in [
        "CASE WHEN NEW.id=1 THEN false ELSE NEW.tick>0 END",
        "NEW.tick=NEW.tick",
    ] {
        let engine = Engine::new();
        exec(&engine, "CREATE TABLE rule_effect_base(id int PRIMARY KEY);
            INSERT INTO rule_effect_base VALUES(1);
            CREATE TABLE rule_effect_log(id int);
            CREATE SEQUENCE rule_effect_ticks;
            CREATE VIEW rule_effect_view AS SELECT id, nextval('rule_effect_ticks') AS tick FROM rule_effect_base");
        exec(
            &engine,
            &format!(
                "CREATE RULE rule_effect AS ON UPDATE TO rule_effect_view WHERE {condition}
            DO ALSO INSERT INTO rule_effect_log VALUES(NEW.id)"
            ),
        );
        assert_eq!(
            exec(&engine, "UPDATE rule_effect_view SET id=id").affected_rows,
            1
        );
        assert_eq!(
            exec(
                &engine,
                "SELECT last_value, is_called FROM rule_effect_ticks"
            )
            .rows[0]["last_value"],
            Value::Int(1)
        );
        assert_eq!(
            exec(&engine, "SELECT is_called FROM rule_effect_ticks").rows[0]["is_called"],
            Value::Bool(true)
        );
        let expected = i64::from(!condition.contains("CASE"));
        assert_eq!(
            exec(&engine, "SELECT count(*) AS n FROM rule_effect_log").rows[0]["n"],
            Value::Int(expected)
        );
    }
}

#[test]
fn volatile_projection_barrier_keeps_other_referenced_errors() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE rule_barrier_base(id int PRIMARY KEY);
        INSERT INTO rule_barrier_base VALUES(1);
        CREATE TABLE rule_barrier_log(id int);
        CREATE SEQUENCE rule_barrier_ticks;
        CREATE VIEW rule_barrier_view AS
          SELECT id, 1/(id-id) AS boom, nextval('rule_barrier_ticks') AS tick FROM rule_barrier_base;
        CREATE RULE rule_barrier AS ON UPDATE TO rule_barrier_view
          WHERE CASE WHEN NEW.id=1 THEN false ELSE NEW.boom>0 OR NEW.tick>0 END
          DO ALSO INSERT INTO rule_barrier_log VALUES(NEW.id)");
    let error = engine
        .sql("UPDATE rule_barrier_view SET id=id", &[])
        .unwrap_err();
    assert!(error.to_string().contains("division by zero"));
    assert_eq!(
        exec(&engine, "SELECT id FROM rule_barrier_base").rows[0]["id"],
        Value::Int(1)
    );
}

#[test]
fn projected_scalar_subqueries_and_whole_rows_remain_under_case() {
    for projection in ["1/(id-id)", "(SELECT 1/(id-id))"] {
        for condition in [
            "CASE WHEN NEW.id=1 THEN false ELSE NEW IS NOT NULL END",
            "(SELECT CASE WHEN NEW.id=1 THEN false ELSE NEW.boom>0 END)",
        ] {
            let engine = Engine::new();
            exec(
                &engine,
                "CREATE TABLE rule_nested_base(id int PRIMARY KEY);
                INSERT INTO rule_nested_base VALUES(1);
                CREATE TABLE rule_nested_log(id int)",
            );
            exec(&engine, &format!("CREATE VIEW rule_nested_view AS SELECT id, {projection} AS boom FROM rule_nested_base;
                CREATE RULE rule_nested AS ON UPDATE TO rule_nested_view WHERE {condition}
                  DO ALSO INSERT INTO rule_nested_log VALUES(NEW.id)"));
            let result = engine
                .sql("UPDATE rule_nested_view SET id=id", &[])
                .unwrap_or_else(|error| panic!("{projection}; {condition}: {error}"));
            assert_eq!(result.affected_rows, 1);
            assert!(exec(&engine, "SELECT * FROM rule_nested_log")
                .rows
                .is_empty());
            let error = engine
                .sql("UPDATE rule_nested_view SET id=2", &[])
                .unwrap_err();
            assert!(error.to_string().contains("division by zero"));
        }
    }
}

#[test]
fn unused_volatile_subqueries_do_not_force_a_skipped_direct_condition() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rule_unused_base(id int PRIMARY KEY);
        INSERT INTO rule_unused_base VALUES(1);
        CREATE TABLE rule_unused_log(id int);
        CREATE SEQUENCE rule_unused_ticks;
        CREATE VIEW rule_unused_view AS SELECT id, (SELECT 1/(id-id)) AS boom,
            (SELECT nextval('rule_unused_ticks')) AS tick FROM rule_unused_base;
        CREATE RULE rule_unused AS ON UPDATE TO rule_unused_view
            WHERE CASE WHEN NEW.id=1 THEN false ELSE NEW.boom>0 END
            DO ALSO INSERT INTO rule_unused_log VALUES(NEW.id)",
    );
    assert_eq!(
        exec(&engine, "UPDATE rule_unused_view SET id=id").affected_rows,
        1
    );
    assert_eq!(
        exec(&engine, "SELECT is_called FROM rule_unused_ticks").rows[0]["is_called"],
        Value::Bool(false)
    );
}

#[test]
fn condition_local_whole_rows_and_columns_shadow_event_rows() {
    for condition in [
        "(SELECT new IS NULL FROM (VALUES(NULL::int)) AS new(x))",
        "(SELECT new IS NULL FROM (VALUES(NULL::int)) AS local(new))",
        "(SELECT new.* IS NULL FROM (VALUES(NULL::int)) AS new(x))",
    ] {
        let engine = Engine::new();
        exec(
            &engine,
            "CREATE TABLE rule_shadow_base(id int PRIMARY KEY);
            INSERT INTO rule_shadow_base VALUES(1);
            CREATE TABLE rule_shadow_log(id int)",
        );
        exec(&engine, &format!("CREATE RULE rule_shadow AS ON UPDATE TO rule_shadow_base WHERE {condition} DO ALSO INSERT INTO rule_shadow_log VALUES(NEW.id)"));
        exec(&engine, "UPDATE rule_shadow_base SET id=id");
        assert_eq!(
            exec(&engine, "SELECT count(*) AS n FROM rule_shadow_log").rows[0]["n"],
            Value::Int(1),
            "{condition}"
        );
    }
}
