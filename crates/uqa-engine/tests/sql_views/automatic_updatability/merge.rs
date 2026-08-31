//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

pub(super) fn assert_automatic_view_merge_rewrites_all_actions() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE merge_view_base (
           id INTEGER PRIMARY KEY,
           value INTEGER,
           hidden TEXT DEFAULT 'defaulted'
         );
         INSERT INTO merge_view_base VALUES
           (1, 10, 'one'), (2, 20, 'two'), (4, 140, 'outside');
         CREATE VIEW merge_view_inner (item_id, value, computed) AS
           SELECT id, value, value + 1 FROM merge_view_base;
         CREATE VIEW merge_view_outer (id, visible_value, doubled_computed) AS
           SELECT item_id, value, computed * 2
           FROM merge_view_inner
           WHERE value < 100;
         CREATE TABLE merge_view_source (id INTEGER, value INTEGER);
         INSERT INTO merge_view_source VALUES (1, 30), (3, 40)",
    );
    let result = exec(
        &engine,
        "MERGE INTO merge_view_outer AS target
         USING merge_view_source AS source
         ON target.id = source.id
         WHEN MATCHED THEN UPDATE SET visible_value = source.value
         WHEN NOT MATCHED THEN
           INSERT (id, visible_value) VALUES (source.id, source.value)
         WHEN NOT MATCHED BY SOURCE THEN DELETE
         RETURNING merge_action() AS action,
           source.id AS source_id,
           target.id AS target_id,
           target.visible_value AS value,
           target.doubled_computed AS computed,
           old.visible_value AS old_value,
           new.visible_value AS new_value",
    );
    assert_eq!(result.affected_rows, 3);
    let update = result
        .rows
        .iter()
        .find(|row| row["action"] == Value::Str("UPDATE".into()))
        .unwrap();
    assert_eq!(update["source_id"], Value::Int(1));
    assert_eq!(update["target_id"], Value::Int(1));
    assert_eq!(update["value"], Value::Int(30));
    assert_eq!(update["computed"], Value::Int(62));
    assert_eq!(update["old_value"], Value::Int(10));
    assert_eq!(update["new_value"], Value::Int(30));
    let insert = result
        .rows
        .iter()
        .find(|row| row["action"] == Value::Str("INSERT".into()))
        .unwrap();
    assert_eq!(insert["source_id"], Value::Int(3));
    assert_eq!(insert["target_id"], Value::Int(3));
    assert_eq!(insert["value"], Value::Int(40));
    assert_eq!(insert["computed"], Value::Int(82));
    assert_eq!(insert["old_value"], Value::Null);
    assert_eq!(insert["new_value"], Value::Int(40));
    let delete = result
        .rows
        .iter()
        .find(|row| row["action"] == Value::Str("DELETE".into()))
        .unwrap();
    assert_eq!(delete["source_id"], Value::Null);
    assert_eq!(delete["target_id"], Value::Int(2));
    assert_eq!(delete["value"], Value::Int(20));
    assert_eq!(delete["old_value"], Value::Int(20));
    assert_eq!(delete["new_value"], Value::Null);
    let state = exec(
        &engine,
        "SELECT id, value, hidden FROM merge_view_base ORDER BY id",
    );
    assert_eq!(state.rows.len(), 3);
    assert_eq!(state.rows[0]["id"], Value::Int(1));
    assert_eq!(state.rows[0]["value"], Value::Int(30));
    assert_eq!(state.rows[0]["hidden"], Value::Str("one".into()));
    assert_eq!(state.rows[1]["id"], Value::Int(3));
    assert_eq!(state.rows[1]["value"], Value::Int(40));
    assert_eq!(state.rows[1]["hidden"], Value::Str("defaulted".into()));
    assert_eq!(state.rows[2]["id"], Value::Int(4));
    assert_eq!(state.rows[2]["value"], Value::Int(140));
}

pub(super) fn assert_automatic_view_merge_filters_target_rows() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE merge_filter_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO merge_filter_base VALUES (1, 10), (2, -20), (3, 30);
         CREATE VIEW merge_filter_view AS
           SELECT id, value FROM merge_filter_base WHERE value > 0;
         CREATE TABLE merge_filter_source (id INTEGER, value INTEGER);
         INSERT INTO merge_filter_source VALUES (1, -11)",
    );
    assert_eq!(
        exec(
            &engine,
            "MERGE INTO merge_filter_view AS target
             USING merge_filter_source AS source
             ON target.id = source.id
             WHEN MATCHED THEN UPDATE SET value = source.value
             WHEN NOT MATCHED BY SOURCE THEN DELETE"
        )
        .affected_rows,
        2
    );
    let state = exec(
        &engine,
        "SELECT id, value FROM merge_filter_base ORDER BY id",
    );
    assert_eq!(state.rows.len(), 2);
    assert_eq!(state.rows[0]["id"], Value::Int(1));
    assert_eq!(state.rows[0]["value"], Value::Int(-11));
    assert_eq!(state.rows[1]["id"], Value::Int(2));
    assert_eq!(state.rows[1]["value"], Value::Int(-20));
}

fn automatic_merge_check_engine() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE merge_check_base (id INTEGER PRIMARY KEY, value INTEGER);
         INSERT INTO merge_check_base VALUES (1, 10);
         CREATE VIEW merge_check_view AS
           SELECT id, value, value + 1 AS computed
           FROM merge_check_base
           WHERE value > 0
           WITH CASCADED CHECK OPTION;
         CREATE FUNCTION merge_check_mutate() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.value = 40 THEN
             NEW.value := -40;
           END IF;
           RETURN NEW;
         END;
         $$;
         CREATE TRIGGER merge_check_mutate_before
           BEFORE UPDATE ON merge_check_base
           FOR EACH ROW EXECUTE FUNCTION merge_check_mutate();
         CREATE TABLE merge_check_source (id INTEGER, value INTEGER);
         INSERT INTO merge_check_source VALUES (1, -1), (2, -2)",
    );
    engine
}

pub(super) fn assert_automatic_view_merge_check_options() {
    let engine = automatic_merge_check_engine();
    let update = engine
        .sql(
            "MERGE INTO merge_check_view AS target
             USING (SELECT id, value FROM merge_check_source WHERE id = 1) AS source
             ON target.id = source.id
             WHEN MATCHED THEN UPDATE SET value = source.value",
            &[],
        )
        .expect_err("MERGE UPDATE must enforce the view check option");
    assert_eq!(update.sqlstate(), Some("44000"));
    let insert = engine
        .sql(
            "MERGE INTO merge_check_view AS target
             USING (SELECT id, value FROM merge_check_source WHERE id = 2) AS source
             ON target.id = source.id
             WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value)",
            &[],
        )
        .expect_err("MERGE INSERT must enforce the view check option");
    assert_eq!(insert.sqlstate(), Some("44000"));
    exec(
        &engine,
        "UPDATE merge_check_source SET value = 40 WHERE id = 1",
    );
    let post_trigger = engine
        .sql(
            "MERGE INTO merge_check_view AS target
             USING (SELECT id, value FROM merge_check_source WHERE id = 1) AS source
             ON target.id = source.id
             WHEN MATCHED THEN UPDATE SET value = source.value",
            &[],
        )
        .expect_err("MERGE must check the final row after a base BEFORE trigger");
    assert_eq!(post_trigger.sqlstate(), Some("44000"));
    let state = exec(
        &engine,
        "SELECT id, value FROM merge_check_base ORDER BY id",
    );
    assert_eq!(state.rows.len(), 1);
    assert_eq!(state.rows[0]["value"], Value::Int(10));
}

pub(super) fn assert_automatic_view_merge_errors() {
    let engine = automatic_merge_check_engine();
    let computed = engine
        .sql(
            "MERGE INTO merge_check_view AS target
             USING merge_check_source AS source
             ON false
             WHEN NOT MATCHED THEN INSERT (id, computed) VALUES (source.id, source.value)",
            &[],
        )
        .expect_err("MERGE must reject a computed view target column");
    assert_eq!(computed.sqlstate(), Some("0A000"));
    exec(
        &engine,
        "CREATE TABLE merge_rule_log (value INTEGER);
         CREATE RULE merge_check_rule AS ON UPDATE TO merge_check_view
           DO ALSO INSERT INTO merge_rule_log VALUES (NEW.value);
         CREATE MATERIALIZED VIEW merge_materialized_view AS
           SELECT id, value FROM merge_check_base;
         CREATE VIEW merge_aggregate_view AS
           SELECT sum(value)::INTEGER AS value FROM merge_check_base",
    );
    let rule = engine
        .sql(
            "MERGE INTO merge_check_view AS target
             USING merge_check_source AS source
             ON target.id = source.id
             WHEN MATCHED THEN UPDATE SET value = source.value",
            &[],
        )
        .expect_err("PostgreSQL rejects MERGE targets with rewrite rules");
    assert_eq!(rule.sqlstate(), Some("0A000"));
    assert!(rule
        .to_string()
        .contains("cannot execute MERGE on relation"));
    let materialized = engine
        .sql(
            "MERGE INTO merge_materialized_view AS target
             USING merge_check_source AS source
             ON target.id = source.id
             WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value)",
            &[],
        )
        .expect_err("MERGE must reject a materialized-view target by relation kind");
    assert_eq!(materialized.sqlstate(), Some("0A000"));
    assert!(materialized
        .to_string()
        .contains("cannot execute MERGE on relation"));
    let aggregate = engine
        .sql(
            "MERGE INTO merge_aggregate_view AS target
             USING merge_check_source AS source
             ON false
             WHEN NOT MATCHED THEN INSERT (value) VALUES (source.value)",
            &[],
        )
        .expect_err("MERGE action must reject a nonautomatically updatable view");
    assert_eq!(aggregate.sqlstate(), Some("55000"));
    let state = exec(
        &engine,
        "SELECT id, value FROM merge_check_base ORDER BY id",
    );
    assert_eq!(state.rows.len(), 1);
    assert_eq!(state.rows[0]["value"], Value::Int(10));
}
