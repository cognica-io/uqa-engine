//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

pub(super) fn assert_rule_action_cardinality_matches_postgresql_18() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("rule-action-cardinality.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(
            &engine,
            "CREATE TABLE cardinality_update_base (id INTEGER PRIMARY KEY);
             INSERT INTO cardinality_update_base VALUES (1), (2), (3);
             CREATE TABLE cardinality_update_log (marker TEXT);
             CREATE RULE cardinality_update_rule AS ON UPDATE TO cardinality_update_base
               DO ALSO INSERT INTO cardinality_update_log VALUES ('updated');
             CREATE TABLE cardinality_update_source (id INTEGER);
             INSERT INTO cardinality_update_source VALUES (1), (2);
             CREATE TABLE cardinality_direct_base (id INTEGER PRIMARY KEY);
             INSERT INTO cardinality_direct_base VALUES (1), (2), (3);
             CREATE TABLE cardinality_direct_log (marker TEXT);
             CREATE VIEW cardinality_direct_view AS SELECT id FROM cardinality_direct_base;
             CREATE RULE cardinality_direct_rule AS ON UPDATE TO cardinality_direct_view
               DO INSTEAD INSERT INTO cardinality_direct_log VALUES ('updated');
             CREATE TABLE cardinality_delete_base (id INTEGER PRIMARY KEY);
             INSERT INTO cardinality_delete_base VALUES (1), (2), (3);
             CREATE TABLE cardinality_delete_log (marker TEXT);
             CREATE RULE cardinality_delete_rule AS ON DELETE TO cardinality_delete_base
               DO ALSO INSERT INTO cardinality_delete_log VALUES ('deleted');
             CREATE TABLE cardinality_delete_using_base (id INTEGER PRIMARY KEY);
             INSERT INTO cardinality_delete_using_base VALUES (1), (2);
             CREATE TABLE cardinality_delete_source (id INTEGER);
             INSERT INTO cardinality_delete_source VALUES (1), (1), (2), (3);
             CREATE TABLE cardinality_delete_using_log (marker TEXT);
             CREATE RULE cardinality_delete_using_rule AS ON DELETE TO cardinality_delete_using_base
               DO ALSO INSERT INTO cardinality_delete_using_log VALUES ('deleted');
             CREATE TABLE cardinality_direct_delete_base (id INTEGER PRIMARY KEY);
             INSERT INTO cardinality_direct_delete_base VALUES (1), (2);
             CREATE VIEW cardinality_direct_delete_view AS SELECT id FROM cardinality_direct_delete_base;
             CREATE TABLE cardinality_direct_delete_log (marker TEXT);
             CREATE RULE cardinality_direct_delete_rule AS ON DELETE TO cardinality_direct_delete_view
               DO INSTEAD INSERT INTO cardinality_direct_delete_log VALUES ('deleted')",
        );
    }
    let engine = Engine::open(&database).unwrap();
    assert_plain_update_rule_action_cardinality(&engine);
    assert_update_from_rule_action_cardinality(&engine);
    assert_plain_delete_rule_action_cardinality(&engine);
    assert_delete_using_rule_action_cardinality(&engine);
    assert_direct_view_delete_using_rule_action_cardinality(&engine);
}

fn assert_plain_update_rule_action_cardinality(engine: &Engine) {
    assert_eq!(
        exec(engine, "UPDATE cardinality_update_base SET id = id").affected_rows,
        3
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_update_log"
        )
        .rows[0]["total"],
        Value::Int(1)
    );
    exec(engine, "DELETE FROM cardinality_update_log");
}

fn assert_update_from_rule_action_cardinality(engine: &Engine) {
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_update_base SET id = id WHERE false"
        )
        .affected_rows,
        0
    );
    assert!(exec(engine, "SELECT * FROM cardinality_update_log")
        .rows
        .is_empty());
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_update_base SET id = id WHERE id > 0"
        )
        .affected_rows,
        3
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_update_log"
        )
        .rows[0]["total"],
        Value::Int(3)
    );
    exec(engine, "DELETE FROM cardinality_update_log");
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_update_base AS target SET id = target.id
             FROM cardinality_update_source AS source"
        )
        .affected_rows,
        3
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_update_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
    exec(engine, "DELETE FROM cardinality_update_log");
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_update_base AS target SET id = target.id
             FROM cardinality_update_source AS source WHERE target.id = 1"
        )
        .affected_rows,
        1
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_update_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
}

fn assert_plain_delete_rule_action_cardinality(engine: &Engine) {
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_direct_view AS target SET id = target.id
             FROM cardinality_update_source AS source"
        )
        .affected_rows,
        0
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_direct_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
    exec(engine, "DELETE FROM cardinality_direct_log");
    assert_eq!(
        exec(
            engine,
            "UPDATE cardinality_direct_view AS target SET id = target.id
             FROM cardinality_update_source AS source WHERE target.id = 1"
        )
        .affected_rows,
        0
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_direct_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
    assert_eq!(
        exec(engine, "DELETE FROM cardinality_delete_base WHERE id > 1").affected_rows,
        2
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_delete_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
    exec(engine, "DELETE FROM cardinality_delete_log");
    assert_eq!(
        exec(engine, "DELETE FROM cardinality_delete_base WHERE false").affected_rows,
        0
    );
    assert!(exec(engine, "SELECT * FROM cardinality_delete_log")
        .rows
        .is_empty());
}

fn assert_delete_using_rule_action_cardinality(engine: &Engine) {
    assert_eq!(
        exec(
            engine,
            "DELETE FROM cardinality_delete_using_base AS target
             USING cardinality_delete_source AS source"
        )
        .affected_rows,
        2
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_delete_using_log"
        )
        .rows[0]["total"],
        Value::Int(4)
    );
    exec(
        engine,
        "INSERT INTO cardinality_delete_using_base VALUES (1), (2);
         DELETE FROM cardinality_delete_using_log",
    );
    assert_eq!(
        exec(
            engine,
            "DELETE FROM cardinality_delete_using_base AS target
             USING cardinality_delete_source AS source WHERE source.id > 1"
        )
        .affected_rows,
        2
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_delete_using_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
    exec(
        engine,
        "INSERT INTO cardinality_delete_using_base VALUES (1), (2);
         DELETE FROM cardinality_delete_using_log",
    );
    assert_eq!(
        exec(
            engine,
            "DELETE FROM cardinality_delete_using_base AS target
             USING cardinality_delete_source AS source WHERE target.id = source.id"
        )
        .affected_rows,
        2
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_delete_using_log"
        )
        .rows[0]["total"],
        Value::Int(3)
    );
    exec(engine, "DELETE FROM cardinality_delete_using_log");
    assert_eq!(
        exec(
            engine,
            "DELETE FROM cardinality_delete_using_base AS target
             USING cardinality_delete_source AS source WHERE source.id > 1"
        )
        .affected_rows,
        0
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_delete_using_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
}

fn assert_direct_view_delete_using_rule_action_cardinality(engine: &Engine) {
    for (sql, expected) in [
        (
            "DELETE FROM cardinality_direct_delete_view AS target USING cardinality_delete_source AS source",
            4,
        ),
        (
            "DELETE FROM cardinality_direct_delete_view AS target USING cardinality_delete_source AS source WHERE source.id > 1",
            2,
        ),
        (
            "DELETE FROM cardinality_direct_delete_view AS target USING cardinality_delete_source AS source WHERE target.id = source.id",
            3,
        ),
    ] {
        exec(engine, "DELETE FROM cardinality_direct_delete_log");
        assert_eq!(exec(engine, sql).affected_rows, 0);
        assert_eq!(
            exec(
                engine,
                "SELECT count(*) AS total FROM cardinality_direct_delete_log"
            )
            .rows[0]["total"],
            Value::Int(expected)
        );
    }
    exec(
        engine,
        "TRUNCATE cardinality_direct_delete_base;
         DELETE FROM cardinality_direct_delete_log",
    );
    assert_eq!(
        exec(
            engine,
            "DELETE FROM cardinality_direct_delete_view AS target
             USING cardinality_delete_source AS source WHERE source.id > 1"
        )
        .affected_rows,
        0
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM cardinality_direct_delete_log"
        )
        .rows[0]["total"],
        Value::Int(2)
    );
}

pub(super) fn assert_rule_suppression_defers_expressions_and_statement_triggers() {
    let engine = Engine::new();
    assert_suppressed_rules_defer_unused_expressions(&engine);
    assert_rules_can_consume_supplied_computed_columns(&engine);
    assert_multiple_instead_rules_use_the_final_action_count(&engine);
    assert_unconditional_instead_rules_suppress_view_triggers(&engine);
}

fn assert_suppressed_rules_defer_unused_expressions(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE lazy_rule_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO lazy_rule_base VALUES (1, 10);
         CREATE VIEW lazy_insert_rule_view AS SELECT id, value FROM lazy_rule_base;
         CREATE VIEW lazy_update_rule_view AS SELECT id, value FROM lazy_rule_base;
         CREATE RULE lazy_insert_rule AS ON INSERT TO lazy_insert_rule_view DO INSTEAD NOTHING;
         CREATE RULE lazy_update_rule AS ON UPDATE TO lazy_update_rule_view DO INSTEAD NOTHING",
    );
    let inserted = exec(
        engine,
        "INSERT INTO lazy_insert_rule_view VALUES (2, 1 / 0)",
    );
    assert_eq!(inserted.affected_rows, 0);
    let updated = exec(
        engine,
        "UPDATE lazy_update_rule_view SET value = 1 / 0 WHERE id = 1",
    );
    assert_eq!(updated.affected_rows, 0);
    assert_eq!(
        exec(engine, "SELECT value FROM lazy_rule_base WHERE id = 1").rows[0]["value"],
        Value::Int(10)
    );
}

fn assert_rules_can_consume_supplied_computed_columns(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE computed_rule_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO computed_rule_base VALUES (1, 2);
         CREATE TABLE computed_rule_log (event TEXT, id INTEGER, doubled INTEGER);
         CREATE VIEW computed_insert_rule_view AS
           SELECT id, value * 2 AS doubled FROM computed_rule_base;
         CREATE VIEW computed_update_rule_view AS
           SELECT id, value * 2 AS doubled FROM computed_rule_base;
         CREATE RULE computed_insert_rule AS ON INSERT TO computed_insert_rule_view
           DO INSTEAD INSERT INTO computed_rule_log VALUES ('insert', NEW.id, NEW.doubled);
         CREATE RULE computed_update_rule AS ON UPDATE TO computed_update_rule_view
           DO INSTEAD INSERT INTO computed_rule_log VALUES ('update', NEW.id, NEW.doubled)",
    );
    assert_eq!(
        exec(
            engine,
            "INSERT INTO computed_insert_rule_view VALUES (2, 8)"
        )
        .affected_rows,
        1
    );
    assert_eq!(
        exec(
            engine,
            "UPDATE computed_update_rule_view SET doubled = 12 WHERE id = 1"
        )
        .affected_rows,
        0
    );
    let computed = exec(
        engine,
        "SELECT event, id, doubled FROM computed_rule_log ORDER BY event",
    );
    assert_eq!(computed.rows[0]["event"], Value::Str("insert".into()));
    assert_eq!(computed.rows[0]["doubled"], Value::Int(8));
    assert_eq!(computed.rows[1]["event"], Value::Str("update".into()));
    assert_eq!(computed.rows[1]["doubled"], Value::Int(12));
}

fn assert_multiple_instead_rules_use_the_final_action_count(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE multi_rule_base (id INTEGER);
         CREATE TABLE multi_rule_first (id INTEGER);
         CREATE TABLE multi_rule_second (id INTEGER);
         CREATE VIEW multi_rule_view AS SELECT id FROM multi_rule_base;
         CREATE RULE multi_rule_a AS ON INSERT TO multi_rule_view
           DO INSTEAD INSERT INTO multi_rule_first VALUES (NEW.id);
         CREATE RULE multi_rule_b AS ON INSERT TO multi_rule_view
           DO INSTEAD INSERT INTO multi_rule_second VALUES (NEW.id)",
    );
    let multiple = exec(engine, "INSERT INTO multi_rule_view VALUES (1), (2)");
    assert_eq!(multiple.affected_rows, 2);
    assert_eq!(
        exec(engine, "SELECT count(*) AS total FROM multi_rule_first").rows[0]["total"],
        Value::Int(2)
    );
    assert_eq!(
        exec(engine, "SELECT count(*) AS total FROM multi_rule_second").rows[0]["total"],
        Value::Int(2)
    );
}

fn assert_unconditional_instead_rules_suppress_view_triggers(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE statement_rule_base (id INTEGER);
         CREATE TABLE statement_rule_log (
           seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           event TEXT NOT NULL
         );
         CREATE VIEW statement_rule_view AS SELECT id FROM statement_rule_base;
         CREATE FUNCTION statement_rule_fn() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO statement_rule_log(event)
             VALUES (TG_WHEN || ':' || TG_LEVEL || ':' || TG_OP);
           RETURN NEW;
         END
         $$;
         CREATE TRIGGER statement_rule_before BEFORE INSERT ON statement_rule_view
           FOR EACH STATEMENT EXECUTE FUNCTION statement_rule_fn();
         CREATE TRIGGER statement_rule_row INSTEAD OF INSERT ON statement_rule_view
           FOR EACH ROW EXECUTE FUNCTION statement_rule_fn();
         CREATE TRIGGER statement_rule_after AFTER INSERT ON statement_rule_view
           FOR EACH STATEMENT EXECUTE FUNCTION statement_rule_fn();
         CREATE RULE statement_rule_instead AS ON INSERT TO statement_rule_view
           DO INSTEAD INSERT INTO statement_rule_log(event) VALUES ('rule')",
    );
    exec(engine, "INSERT INTO statement_rule_view VALUES (1)");
    let trigger_log = exec(engine, "SELECT event FROM statement_rule_log ORDER BY seq");
    assert_eq!(trigger_log.rows.len(), 1);
    assert_eq!(trigger_log.rows[0]["event"], Value::Str("rule".into()));
}

pub(super) fn assert_view_rules_precede_automatic_rewrite() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE view_rule_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         CREATE TABLE view_rule_log (event TEXT NOT NULL, id INTEGER, value INTEGER);
         CREATE VIEW view_rule_instead AS SELECT id, value FROM view_rule_base;
         CREATE RULE view_insert_instead AS ON INSERT TO view_rule_instead
         DO INSTEAD INSERT INTO view_rule_log VALUES ('insert-instead', NEW.id, NEW.value)",
    );
    assert_view_insert_rules(&engine);
    assert_view_update_and_delete_rules(&engine);
    assert_view_rule_returning(&engine);
}

fn assert_view_insert_rules(engine: &Engine) {
    exec(engine, "INSERT INTO view_rule_instead VALUES (1, 10)");
    assert!(exec(engine, "SELECT * FROM view_rule_base").rows.is_empty());
    let insert_log = exec(
        engine,
        "SELECT event, id, value FROM view_rule_log ORDER BY event",
    );
    assert_eq!(
        insert_log.rows[0]["event"],
        Value::Str("insert-instead".into())
    );
    assert_eq!(insert_log.rows[0]["id"], Value::Int(1));
    assert_eq!(insert_log.rows[0]["value"], Value::Int(10));

    exec(
        engine,
        "CREATE VIEW view_rule_also AS SELECT id, value FROM view_rule_base;
         CREATE RULE view_insert_also AS ON INSERT TO view_rule_also
         DO ALSO INSERT INTO view_rule_log VALUES ('insert-also', NEW.id, NEW.value)",
    );
    exec(engine, "INSERT INTO view_rule_also VALUES (2, 20)");
    let stored = exec(engine, "SELECT value FROM view_rule_base WHERE id = 2");
    assert_eq!(stored.rows[0]["value"], Value::Int(20));
    let also_log = exec(
        engine,
        "SELECT value FROM view_rule_log WHERE event = 'insert-also'",
    );
    assert_eq!(also_log.rows[0]["value"], Value::Int(20));
}

fn assert_view_update_and_delete_rules(engine: &Engine) {
    exec(
        engine,
        "INSERT INTO view_rule_base VALUES (3, 30);
         CREATE RULE view_update_instead AS ON UPDATE TO view_rule_instead
         DO INSTEAD INSERT INTO view_rule_log VALUES ('update-instead', OLD.id, NEW.value);
         CREATE RULE view_delete_instead AS ON DELETE TO view_rule_instead
         DO INSTEAD INSERT INTO view_rule_log VALUES ('delete-instead', OLD.id, OLD.value)",
    );
    let updated = exec(
        engine,
        "UPDATE view_rule_instead SET value = 31 WHERE id = 3",
    );
    assert_eq!(updated.affected_rows, 0);
    assert_eq!(
        exec(engine, "SELECT value FROM view_rule_base WHERE id = 3").rows[0]["value"],
        Value::Int(30)
    );
    assert_eq!(
        exec(
            engine,
            "SELECT value FROM view_rule_log WHERE event = 'update-instead'"
        )
        .rows[0]["value"],
        Value::Int(31)
    );
    let deleted = exec(engine, "DELETE FROM view_rule_instead WHERE id = 3");
    assert_eq!(deleted.affected_rows, 0);
    assert_eq!(
        exec(
            engine,
            "SELECT value FROM view_rule_log WHERE event = 'delete-instead'"
        )
        .rows[0]["value"],
        Value::Int(30)
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM view_rule_base WHERE id = 3"
        )
        .rows[0]["total"],
        Value::Int(1)
    );

    exec(
        engine,
        "CREATE RULE view_update_also AS ON UPDATE TO view_rule_also
         DO ALSO INSERT INTO view_rule_log VALUES ('update-also', OLD.id, NEW.value);
         CREATE RULE view_delete_also AS ON DELETE TO view_rule_also
         DO ALSO INSERT INTO view_rule_log VALUES ('delete-also', OLD.id, OLD.value)",
    );
    exec(engine, "UPDATE view_rule_also SET value = 21 WHERE id = 2");
    assert_eq!(
        exec(engine, "SELECT value FROM view_rule_base WHERE id = 2").rows[0]["value"],
        Value::Int(21)
    );
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM view_rule_log WHERE event = 'update-also'"
        )
        .rows[0]["total"],
        Value::Int(1)
    );
    exec(engine, "DELETE FROM view_rule_also WHERE id = 2");
    assert!(exec(engine, "SELECT * FROM view_rule_base WHERE id = 2")
        .rows
        .is_empty());
    assert_eq!(
        exec(
            engine,
            "SELECT count(*) AS total FROM view_rule_log WHERE event = 'delete-also'"
        )
        .rows[0]["total"],
        Value::Int(1)
    );
}

fn assert_view_rule_returning(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE view_rule_return_action (id INTEGER, value INTEGER);
         CREATE VIEW view_rule_returning AS SELECT id, value FROM view_rule_base;
         CREATE RULE view_insert_returning AS ON INSERT TO view_rule_returning
         DO INSTEAD INSERT INTO view_rule_return_action VALUES (NEW.id, NEW.value)
         RETURNING id, value",
    );
    let returned = exec(
        engine,
        "INSERT INTO view_rule_returning VALUES (4, 40) RETURNING id, value",
    );
    assert_eq!(returned.rows[0]["id"], Value::Int(4));
    assert_eq!(returned.rows[0]["value"], Value::Int(40));
    assert!(exec(engine, "SELECT * FROM view_rule_base WHERE id = 4")
        .rows
        .is_empty());
}

pub(super) fn assert_nested_view_rules_run_at_each_rewrite_layer() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_rule_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         CREATE TABLE nested_rule_log (
           seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           event TEXT NOT NULL
         );
         CREATE VIEW nested_rule_inner AS SELECT id, value FROM nested_rule_base;
         CREATE VIEW nested_rule_outer AS SELECT id, value FROM nested_rule_inner;
         CREATE RULE nested_inner_insert AS ON INSERT TO nested_rule_inner
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('inner-insert');
         CREATE RULE nested_outer_insert AS ON INSERT TO nested_rule_outer
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('outer-insert');
         CREATE RULE nested_inner_update AS ON UPDATE TO nested_rule_inner
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('inner-update');
         CREATE RULE nested_outer_update AS ON UPDATE TO nested_rule_outer
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('outer-update');
         CREATE RULE nested_inner_delete AS ON DELETE TO nested_rule_inner
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('inner-delete');
         CREATE RULE nested_outer_delete AS ON DELETE TO nested_rule_outer
           DO ALSO INSERT INTO nested_rule_log(event) VALUES ('outer-delete')",
    );
    exec(&engine, "INSERT INTO nested_rule_outer VALUES (1, 10)");
    exec(
        &engine,
        "UPDATE nested_rule_outer SET value = 11 WHERE id = 1",
    );
    exec(&engine, "DELETE FROM nested_rule_outer WHERE id = 1");
    let log = exec(&engine, "SELECT event FROM nested_rule_log ORDER BY seq");
    assert_eq!(
        log.rows
            .iter()
            .map(|row| row["event"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Str("inner-insert".into()),
            Value::Str("outer-insert".into()),
            Value::Str("outer-update".into()),
            Value::Str("inner-update".into()),
            Value::Str("outer-delete".into()),
            Value::Str("inner-delete".into()),
        ]
    );
    assert!(exec(&engine, "SELECT * FROM nested_rule_base")
        .rows
        .is_empty());
}

pub(super) fn assert_rule_and_trigger_rewrite_order() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rule_trigger_base (id INTEGER PRIMARY KEY);
         CREATE TABLE rule_trigger_log (
           seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           event TEXT NOT NULL
         );
         CREATE FUNCTION rule_trigger_fn() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO rule_trigger_log(event) VALUES ('trigger');
           RETURN NEW;
         END
         $$;
         CREATE VIEW rule_trigger_also AS SELECT id FROM rule_trigger_base;
         CREATE TRIGGER rule_trigger_also_trigger INSTEAD OF INSERT ON rule_trigger_also
           FOR EACH ROW EXECUTE FUNCTION rule_trigger_fn();
         CREATE RULE rule_trigger_also_rule AS ON INSERT TO rule_trigger_also
           DO ALSO INSERT INTO rule_trigger_log(event) VALUES ('rule')",
    );
    exec(&engine, "INSERT INTO rule_trigger_also VALUES (1)");
    let log = exec(&engine, "SELECT event FROM rule_trigger_log ORDER BY seq");
    assert_eq!(
        log.rows
            .iter()
            .map(|row| row["event"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Str("trigger".into()), Value::Str("rule".into())]
    );

    exec(
        &engine,
        "TRUNCATE rule_trigger_log;
         CREATE VIEW rule_trigger_instead AS SELECT id FROM rule_trigger_base;
         CREATE TRIGGER rule_trigger_instead_trigger INSTEAD OF INSERT ON rule_trigger_instead
           FOR EACH ROW EXECUTE FUNCTION rule_trigger_fn();
         CREATE RULE rule_trigger_instead_rule AS ON INSERT TO rule_trigger_instead
           DO INSTEAD INSERT INTO rule_trigger_log(event) VALUES ('rule')",
    );
    exec(&engine, "INSERT INTO rule_trigger_instead VALUES (2)");
    let log = exec(&engine, "SELECT event FROM rule_trigger_log ORDER BY seq");
    assert_eq!(log.rows.len(), 1);
    assert_eq!(log.rows[0]["event"], Value::Str("rule".into()));
}

pub(super) fn assert_update_delete_rule_and_trigger_rewrite_order() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rule_trigger_ud_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         INSERT INTO rule_trigger_ud_base VALUES (1, 10), (2, 20);
         CREATE TABLE rule_trigger_ud_log (
           seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           event TEXT NOT NULL
         );
         CREATE FUNCTION rule_trigger_ud_fn() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO rule_trigger_ud_log(event) VALUES (lower(TG_OP) || '-trigger');
           IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
           RETURN NEW;
         END
         $$;
         CREATE VIEW rule_trigger_ud_view AS SELECT id, value FROM rule_trigger_ud_base;
         CREATE TRIGGER rule_trigger_ud INSTEAD OF UPDATE OR DELETE ON rule_trigger_ud_view
           FOR EACH ROW EXECUTE FUNCTION rule_trigger_ud_fn();
         CREATE RULE rule_trigger_update_also AS ON UPDATE TO rule_trigger_ud_view
           DO ALSO INSERT INTO rule_trigger_ud_log(event) VALUES ('update-rule');
         CREATE RULE rule_trigger_delete_also AS ON DELETE TO rule_trigger_ud_view
           DO ALSO INSERT INTO rule_trigger_ud_log(event) VALUES ('delete-rule')",
    );
    exec(
        &engine,
        "UPDATE rule_trigger_ud_view SET value = 11 WHERE id = 1",
    );
    exec(&engine, "DELETE FROM rule_trigger_ud_view WHERE id = 2");
    let log = exec(
        &engine,
        "SELECT event FROM rule_trigger_ud_log ORDER BY seq",
    );
    assert_eq!(
        log.rows
            .iter()
            .map(|row| row["event"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Str("update-rule".into()),
            Value::Str("update-trigger".into()),
            Value::Str("delete-rule".into()),
            Value::Str("delete-trigger".into()),
        ]
    );

    exec(
        &engine,
        "TRUNCATE rule_trigger_ud_log;
         CREATE VIEW rule_trigger_ud_suppressed AS SELECT id, value FROM rule_trigger_ud_base;
         CREATE TRIGGER rule_trigger_ud_suppressed_trigger
           INSTEAD OF UPDATE OR DELETE ON rule_trigger_ud_suppressed
           FOR EACH ROW EXECUTE FUNCTION rule_trigger_ud_fn();
         CREATE RULE rule_trigger_update_instead AS ON UPDATE TO rule_trigger_ud_suppressed
           DO INSTEAD INSERT INTO rule_trigger_ud_log(event) VALUES ('update-rule');
         CREATE RULE rule_trigger_delete_instead AS ON DELETE TO rule_trigger_ud_suppressed
           DO INSTEAD INSERT INTO rule_trigger_ud_log(event) VALUES ('delete-rule')",
    );
    exec(
        &engine,
        "UPDATE rule_trigger_ud_suppressed SET value = 12 WHERE id = 1",
    );
    exec(
        &engine,
        "DELETE FROM rule_trigger_ud_suppressed WHERE id = 2",
    );
    let log = exec(
        &engine,
        "SELECT event FROM rule_trigger_ud_log ORDER BY seq",
    );
    assert_eq!(
        log.rows
            .iter()
            .map(|row| row["event"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Str("update-rule".into()),
            Value::Str("delete-rule".into()),
        ]
    );
    let base = exec(
        &engine,
        "SELECT id, value FROM rule_trigger_ud_base ORDER BY id",
    );
    assert_eq!(base.rows[0]["value"], Value::Int(10));
    assert_eq!(base.rows[1]["value"], Value::Int(20));
}

pub(super) fn assert_view_rule_actions_with_duplicate_user_ids_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("automatic-view-rule-actions.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE persistent_rule_base (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE persistent_rule_log (event TEXT NOT NULL, id INTEGER, value INTEGER);
             CREATE VIEW persistent_rule_view AS SELECT id, value FROM persistent_rule_base;
             CREATE RULE persistent_update_also AS ON UPDATE TO persistent_rule_view
             DO ALSO INSERT INTO persistent_rule_log VALUES ('update', OLD.id, NEW.value);
             CREATE RULE persistent_delete_also AS ON DELETE TO persistent_rule_view
             DO ALSO INSERT INTO persistent_rule_log VALUES ('delete', OLD.id, OLD.value);
             INSERT INTO persistent_rule_base VALUES (1, 10);
             UPDATE persistent_rule_view SET value = 11 WHERE id = 1",
        );
    }
    {
        let engine = Engine::open(&path).unwrap();
        exec(&engine, "DELETE FROM persistent_rule_view WHERE id = 1");
        let rows = exec(
            &engine,
            "SELECT event, id, value FROM persistent_rule_log ORDER BY event",
        );
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(rows.rows[0]["event"], Value::Str("delete".into()));
        assert_eq!(rows.rows[0]["id"], Value::Int(1));
        assert_eq!(rows.rows[0]["value"], Value::Int(11));
        assert_eq!(rows.rows[1]["event"], Value::Str("update".into()));
        assert_eq!(rows.rows[1]["id"], Value::Int(1));
        assert_eq!(rows.rows[1]["value"], Value::Int(11));
    }
    let engine = Engine::open(&path).unwrap();
    assert_eq!(
        exec(&engine, "SELECT count(*) AS total FROM persistent_rule_log").rows[0]["total"],
        Value::Int(2)
    );
}
