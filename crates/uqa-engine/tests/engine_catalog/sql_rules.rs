//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 rewrite-rule execution and catalog lifecycle coverage.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

fn exec(engine: &Engine, sql: &str) -> uqa_engine::SQLResult {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

fn strings(engine: &Engine, sql: &str, column: &str) -> Vec<String> {
    exec(engine, sql)
        .rows
        .into_iter()
        .map(|row| match row.get(column) {
            Some(Value::Str(value)) => value.clone(),
            other => panic!("expected text column `{column}`, got {other:?}"),
        })
        .collect()
}

#[test]
fn insert_rules_run_by_name_bind_new_and_apply_conditional_instead() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rule_items (id INTEGER PRIMARY KEY, value TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE rule_log (seq BIGSERIAL PRIMARY KEY, message TEXT)",
    );
    exec(
        &engine,
        "CREATE RULE b_log AS ON INSERT TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('b:' || NEW.id || ':' || NEW.value)",
    );
    exec(
        &engine,
        "CREATE RULE a_log AS ON INSERT TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('a:' || NEW.id || ':' || NEW.value)",
    );
    exec(
        &engine,
        "CREATE RULE suppress_two AS ON INSERT TO rule_items WHERE NEW.id = 2 DO INSTEAD NOTHING",
    );

    exec(
        &engine,
        "INSERT INTO rule_items VALUES (1, 'one'), (2, 'two')",
    );

    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM rule_log ORDER BY seq",
            "message"
        ),
        ["a:1:one", "a:2:two", "b:1:one", "b:2:two"]
    );
    assert_eq!(
        exec(&engine, "SELECT id FROM rule_items ORDER BY id")
            .rows
            .iter()
            .map(|row| row.get("id"))
            .collect::<Vec<_>>(),
        [Some(&Value::Int(1))]
    );
}

#[test]
fn rule_row_images_keep_missing_nullable_integers_null() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nullable_rule_items (id INTEGER PRIMARY KEY, optional INTEGER)",
    );
    exec(&engine, "CREATE TABLE nullable_rule_log (value INTEGER)");
    exec(
        &engine,
        "CREATE RULE log_optional AS ON INSERT TO nullable_rule_items DO ALSO INSERT INTO nullable_rule_log VALUES (NEW.optional)",
    );
    exec(&engine, "INSERT INTO nullable_rule_items (id) VALUES (7)");
    assert_eq!(
        exec(&engine, "SELECT value FROM nullable_rule_log").rows[0].get("value"),
        Some(&Value::Null)
    );
}

#[test]
fn insert_and_delete_rule_conditions_resolve_unqualified_target_columns() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE condition_items (id INTEGER)");
    exec(
        &engine,
        "CREATE RULE suppress_small AS ON INSERT TO condition_items WHERE id < 10 DO INSTEAD NOTHING",
    );
    exec(&engine, "INSERT INTO condition_items VALUES (1), (10)");
    assert_eq!(
        exec(&engine, "SELECT id FROM condition_items").rows[0].get("id"),
        Some(&Value::Int(10))
    );
    exec(
        &engine,
        "CREATE RULE retain_ten AS ON DELETE TO condition_items WHERE id = 10 DO INSTEAD NOTHING",
    );
    exec(&engine, "DELETE FROM condition_items");
    assert_eq!(
        exec(&engine, "SELECT id FROM condition_items").rows[0].get("id"),
        Some(&Value::Int(10))
    );
    let ambiguous = engine
        .sql(
            "CREATE RULE ambiguous_update AS ON UPDATE TO condition_items WHERE id = 10 DO INSTEAD NOTHING",
            &[],
        )
        .expect_err("UPDATE exposes both OLD and NEW");
    assert_eq!(ambiguous.sqlstate(), Some("42702"));
}

#[test]
fn update_delete_rules_bind_row_images_and_apply_instead() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rule_items (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE rule_log (seq BIGSERIAL PRIMARY KEY, message TEXT)",
    );
    exec(&engine, "INSERT INTO rule_items VALUES (1, 10), (2, 20)");
    exec(
        &engine,
        "CREATE RULE update_log AS ON UPDATE TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('u:' || OLD.id || ':' || OLD.value || ':' || NEW.value)",
    );
    exec(
        &engine,
        "CREATE RULE keep_two AS ON DELETE TO rule_items WHERE OLD.id = 2 DO INSTEAD NOTHING",
    );
    exec(
        &engine,
        "CREATE RULE delete_log AS ON DELETE TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('d:' || OLD.id || ':' || OLD.value)",
    );

    exec(&engine, "UPDATE rule_items SET value = value + 1");
    exec(&engine, "DELETE FROM rule_items");

    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM rule_log ORDER BY seq",
            "message"
        ),
        ["u:1:10:11", "u:2:20:21", "d:1:11", "d:2:21"]
    );
    assert_eq!(
        exec(&engine, "SELECT id FROM rule_items")
            .rows
            .first()
            .and_then(|row| row.get("id")),
        Some(&Value::Int(2))
    );
}

#[test]
fn insert_select_and_constant_actions_preserve_statement_cardinality() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rule_source (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE rule_items (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE rule_log (seq BIGSERIAL PRIMARY KEY, message TEXT)",
    );
    exec(&engine, "INSERT INTO rule_source VALUES (1, 10), (2, 20)");
    exec(
        &engine,
        "CREATE RULE insert_constant AS ON INSERT TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('insert')",
    );
    exec(
        &engine,
        "CREATE RULE update_constant AS ON UPDATE TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('update')",
    );
    exec(
        &engine,
        "CREATE RULE delete_constant AS ON DELETE TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('delete')",
    );

    exec(
        &engine,
        "INSERT INTO rule_items SELECT id, value FROM rule_source ORDER BY id",
    );
    exec(&engine, "UPDATE rule_items SET value = value + 1");
    exec(&engine, "DELETE FROM rule_items");

    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM rule_log ORDER BY seq",
            "message"
        ),
        ["insert", "insert", "update", "delete"]
    );
}

#[test]
fn rule_actions_execute_once_over_the_qualified_row_set() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE set_rule_source (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE set_rule_target (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(&engine, "CREATE TABLE set_rule_rows (id INTEGER)");
    exec(
        &engine,
        "CREATE TABLE set_rule_statements (seq BIGSERIAL PRIMARY KEY, event TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO set_rule_source VALUES (1, 10), (2, 20)",
    );
    exec(&engine, "INSERT INTO set_rule_target VALUES (1, 0)");
    exec(
        &engine,
        "CREATE FUNCTION log_rule_statement() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO set_rule_statements(event) VALUES (TG_OP); RETURN NULL; END $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER log_rule_insert AFTER INSERT ON set_rule_rows FOR EACH STATEMENT EXECUTE FUNCTION log_rule_statement()",
    );
    exec(
        &engine,
        "CREATE TRIGGER log_rule_update AFTER UPDATE ON set_rule_target FOR EACH STATEMENT EXECUTE FUNCTION log_rule_statement()",
    );
    exec(
        &engine,
        "CREATE RULE a_insert_rows AS ON UPDATE TO set_rule_source DO ALSO INSERT INTO set_rule_rows VALUES (NEW.id)",
    );
    exec(
        &engine,
        "CREATE RULE b_update_once AS ON UPDATE TO set_rule_source DO ALSO UPDATE set_rule_target SET value = NEW.id WHERE NEW.id = 1",
    );

    exec(&engine, "UPDATE set_rule_source SET value = value + 1");

    assert_eq!(
        exec(&engine, "SELECT id FROM set_rule_rows ORDER BY id")
            .rows
            .iter()
            .map(|row| row.get("id"))
            .collect::<Vec<_>>(),
        [Some(&Value::Int(1)), Some(&Value::Int(2))]
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM set_rule_target").rows[0].get("value"),
        Some(&Value::Int(1))
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT event FROM set_rule_statements ORDER BY seq",
            "event"
        ),
        ["INSERT", "UPDATE"]
    );
}

#[test]
fn empty_event_sets_still_execute_rule_actions_once() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE empty_rule_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(&engine, "CREATE TABLE empty_rule_constant (value TEXT)");
    exec(&engine, "CREATE TABLE empty_rule_rows (id INTEGER)");
    exec(&engine, "CREATE TABLE empty_rule_statements (event TEXT)");
    exec(
        &engine,
        "CREATE FUNCTION log_empty_rule_statement() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO empty_rule_statements VALUES (TG_OP); RETURN NULL; END $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER empty_rule_statement AFTER INSERT ON empty_rule_rows FOR EACH STATEMENT EXECUTE FUNCTION log_empty_rule_statement()",
    );
    exec(
        &engine,
        "CREATE RULE a_empty_constant AS ON UPDATE TO empty_rule_event DO ALSO INSERT INTO empty_rule_constant VALUES ('ran')",
    );
    exec(
        &engine,
        "CREATE RULE b_empty_rows AS ON UPDATE TO empty_rule_event DO ALSO INSERT INTO empty_rule_rows VALUES (NEW.id)",
    );

    let result = exec(&engine, "UPDATE empty_rule_event SET value = value + 1");

    assert_eq!(result.affected_rows, 0);
    assert_eq!(
        exec(&engine, "SELECT value FROM empty_rule_constant").value_at(0, 0),
        Some(&Value::Str("ran".into()))
    );
    assert!(exec(&engine, "SELECT id FROM empty_rule_rows")
        .rows
        .is_empty());
    assert_eq!(
        exec(&engine, "SELECT event FROM empty_rule_statements").value_at(0, 0),
        Some(&Value::Str("INSERT".into()))
    );
}

#[test]
fn rule_action_lateral_sources_see_the_set_oriented_event_relation() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE lateral_rule_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(&engine, "CREATE TABLE lateral_rule_log (value INTEGER)");
    exec(
        &engine,
        "INSERT INTO lateral_rule_event VALUES (1, 10), (2, 20)",
    );
    exec(
        &engine,
        "CREATE RULE lateral_rule AS ON UPDATE TO lateral_rule_event DO ALSO INSERT INTO lateral_rule_log SELECT item.value FROM LATERAL (SELECT NEW.value AS value) AS item",
    );

    exec(&engine, "UPDATE lateral_rule_event SET value = value + 1");

    assert_eq!(
        exec(&engine, "SELECT value FROM lateral_rule_log ORDER BY value")
            .rows
            .iter()
            .map(|row| row.get("value"))
            .collect::<Vec<_>>(),
        [Some(&Value::Int(11)), Some(&Value::Int(21))]
    );
}

#[test]
fn rule_action_internal_source_names_cannot_shadow_user_aliases() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE alias_rule_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(&engine, "CREATE TABLE alias_rule_source (__new_0 INTEGER)");
    exec(
        &engine,
        "CREATE TABLE alias_rule_log (event_id INTEGER, source_value INTEGER)",
    );
    exec(&engine, "INSERT INTO alias_rule_event VALUES (1, 10)");
    exec(&engine, "INSERT INTO alias_rule_source VALUES (999)");
    exec(
        &engine,
        "CREATE RULE alias_rule AS ON UPDATE TO alias_rule_event DO ALSO INSERT INTO alias_rule_log SELECT NEW.id, __uqa_rule_rows_0_0.__new_0 FROM alias_rule_source AS __uqa_rule_rows_0_0",
    );

    exec(&engine, "UPDATE alias_rule_event SET value = value + 1");

    let result = exec(&engine, "SELECT event_id, source_value FROM alias_rule_log");
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(999)));
}

#[test]
fn rule_provider_returning_is_lazy_and_uses_only_action_row_images() {
    let engine = Engine::new();
    exec(&engine, "CREATE SEQUENCE rule_returning_side_effect");
    exec(&engine, "CREATE TABLE lazy_returning_event (id BIGINT)");
    exec(&engine, "CREATE TABLE lazy_returning_action (id BIGINT)");
    exec(
        &engine,
        "CREATE RULE lazy_returning_rule AS ON INSERT TO lazy_returning_event DO INSTEAD INSERT INTO lazy_returning_action VALUES (NEW.id) RETURNING nextval('rule_returning_side_effect')",
    );

    exec(&engine, "INSERT INTO lazy_returning_event VALUES (10)");

    assert_eq!(
        exec(&engine, "SELECT nextval('rule_returning_side_effect')").value_at(0, 0),
        Some(&Value::Int(1))
    );
    assert_eq!(
        exec(&engine, "SELECT id FROM lazy_returning_action").value_at(0, 0),
        Some(&Value::Int(10))
    );

    exec(
        &engine,
        "CREATE TABLE action_image_event (x INTEGER, y INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE action_image_target (id INTEGER, value INTEGER)",
    );
    exec(
        &engine,
        "INSERT INTO action_image_event VALUES (1, 10), (2, 20)",
    );
    exec(
        &engine,
        "CREATE RULE action_image_rule AS ON UPDATE TO action_image_event DO INSTEAD INSERT INTO action_image_target VALUES (42, 43) RETURNING NEW.id, NEW.value",
    );

    let result = exec(
        &engine,
        "UPDATE action_image_event SET y = y + 1 RETURNING x, y",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(42)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(43)));
    assert_eq!(
        exec(&engine, "SELECT count(*) FROM action_image_target").value_at(0, 0),
        Some(&Value::Int(1))
    );

    exec(&engine, "CREATE SEQUENCE literal_returning_side_effect");
    exec(&engine, "CREATE TABLE literal_returning_event (id BIGINT)");
    exec(&engine, "CREATE TABLE literal_returning_action (id BIGINT)");
    exec(
        &engine,
        "CREATE RULE literal_returning_rule AS ON INSERT TO literal_returning_event DO INSTEAD INSERT INTO literal_returning_action VALUES (NEW.id) RETURNING nextval('literal_returning_side_effect')",
    );
    let literal = exec(
        &engine,
        "INSERT INTO literal_returning_event VALUES (10) RETURNING 42 AS answer",
    );
    assert_eq!(literal.value_at(0, 0), Some(&Value::Int(42)));
    assert_eq!(
        exec(&engine, "SELECT nextval('literal_returning_side_effect')").value_at(0, 0),
        Some(&Value::Int(1))
    );

    exec(&engine, "CREATE SEQUENCE all_images_side_effect");
    exec(&engine, "CREATE TABLE all_images_event (id BIGINT)");
    exec(&engine, "CREATE TABLE all_images_action (id BIGINT)");
    exec(
        &engine,
        "CREATE RULE all_images_rule AS ON INSERT TO all_images_event DO INSTEAD INSERT INTO all_images_action VALUES (NEW.id) RETURNING nextval('all_images_side_effect')",
    );
    let all_images = exec(
        &engine,
        "INSERT INTO all_images_event VALUES (10) RETURNING id AS current_id, old.id AS old_id, new.id AS new_id",
    );
    assert_eq!(all_images.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(all_images.value_at(0, 1), Some(&Value::Null));
    assert_eq!(all_images.value_at(0, 2), Some(&Value::Int(2)));

    exec(&engine, "CREATE SEQUENCE missing_new_side_effect");
    exec(&engine, "CREATE TABLE missing_new_event (id BIGINT)");
    exec(&engine, "CREATE TABLE missing_new_action (id BIGINT)");
    exec(&engine, "INSERT INTO missing_new_event VALUES (10)");
    exec(&engine, "INSERT INTO missing_new_action VALUES (10)");
    exec(
        &engine,
        "CREATE RULE missing_new_rule AS ON DELETE TO missing_new_event DO INSTEAD DELETE FROM missing_new_action WHERE id = OLD.id RETURNING nextval('missing_new_side_effect')",
    );
    let missing_new = exec(
        &engine,
        "DELETE FROM missing_new_event RETURNING new.id AS new_id",
    );
    assert_eq!(missing_new.value_at(0, 0), Some(&Value::Null));
    assert_eq!(
        exec(&engine, "SELECT nextval('missing_new_side_effect')").value_at(0, 0),
        Some(&Value::Int(1))
    );
}

#[test]
fn rule_pseudo_relations_yield_to_local_sql_scopes() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE shadow_rule_event (id INTEGER)");
    exec(&engine, "CREATE TABLE shadow_rule_log (id INTEGER)");
    let duplicate = engine
        .sql(
            "CREATE RULE shadow_cte_rule AS ON INSERT TO shadow_rule_event DO ALSO WITH old AS (SELECT 77 AS id) INSERT INTO shadow_rule_log SELECT old.id FROM old",
            &[],
        )
        .unwrap_err();
    assert_eq!(duplicate.sqlstate(), Some("42712"));
    assert!(duplicate
        .to_string()
        .contains("table name \"old\" specified more than once"));
    exec(
        &engine,
        "CREATE TABLE shadow_conflict_target (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE RULE shadow_target_rule AS ON INSERT TO shadow_rule_event DO ALSO INSERT INTO shadow_conflict_target AS new VALUES (NEW.id, 1) ON CONFLICT (id) DO UPDATE SET value = new.value + 1",
    );
    exec(&engine, "INSERT INTO shadow_rule_event VALUES (1)");
    exec(&engine, "INSERT INTO shadow_rule_event VALUES (1)");
    assert!(exec(&engine, "SELECT id FROM shadow_rule_log")
        .rows
        .is_empty());
    assert_eq!(
        exec(&engine, "SELECT value FROM shadow_conflict_target").value_at(0, 0),
        Some(&Value::Int(2))
    );

    exec(&engine, "CREATE TABLE nested_alias_event (id INTEGER)");
    exec(&engine, "CREATE TABLE nested_alias_action (id INTEGER)");
    exec(
        &engine,
        "CREATE RULE nested_alias_provider AS ON INSERT TO nested_alias_event DO INSTEAD INSERT INTO nested_alias_action VALUES (NEW.id) RETURNING (SELECT old.id FROM (SELECT 99 AS id) AS old)",
    );
    let nested = exec(
        &engine,
        "INSERT INTO nested_alias_event VALUES (1) RETURNING id",
    );
    assert_eq!(nested.value_at(0, 0), Some(&Value::Int(99)));
}

#[test]
fn rule_action_query_scope_restrictions_match_postgresql() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE scoped_rule_event (id INTEGER)");
    exec(
        &engine,
        "CREATE TABLE scoped_rule_target (id INTEGER PRIMARY KEY)",
    );

    let set_operation = engine
        .sql(
            "CREATE RULE set_operation_rule AS ON INSERT TO scoped_rule_event DO ALSO INSERT INTO scoped_rule_target SELECT NEW.id UNION ALL SELECT 999",
            &[],
        )
        .expect_err("set-operation members cannot reference the event relation");
    assert_eq!(set_operation.sqlstate(), Some("42P10"));

    exec(
        &engine,
        "CREATE RULE constant_set_operation_rule AS ON INSERT TO scoped_rule_event DO ALSO INSERT INTO scoped_rule_target SELECT 1000 UNION ALL SELECT 1001",
    );
    let conditional_set_operation = engine
        .sql(
            "CREATE RULE conditional_set_operation_rule AS ON UPDATE TO scoped_rule_event WHERE NEW.id > 0 DO ALSO INSERT INTO scoped_rule_target SELECT 1002 UNION ALL SELECT 1003",
            &[],
        )
        .expect_err("conditional set-operation actions are rejected when the rule is created");
    assert_eq!(conditional_set_operation.sqlstate(), Some("0A000"));
    exec(&engine, "INSERT INTO scoped_rule_event VALUES (1)");
    assert_eq!(
        exec(&engine, "SELECT id FROM scoped_rule_target ORDER BY id")
            .rows
            .iter()
            .map(|row| row.get("id"))
            .collect::<Vec<_>>(),
        [Some(&Value::Int(1000)), Some(&Value::Int(1001))]
    );
    exec(&engine, "DELETE FROM scoped_rule_event");
    exec(&engine, "DELETE FROM scoped_rule_target");
    let rewritten_set_operation = engine
        .sql("INSERT INTO scoped_rule_event VALUES (1), (2)", &[])
        .expect_err("INSERT rewrite makes a set-operation action conditional");
    assert_eq!(rewritten_set_operation.sqlstate(), Some("0A000"));
    assert!(exec(&engine, "SELECT * FROM scoped_rule_event")
        .rows
        .is_empty());
    assert!(exec(&engine, "SELECT * FROM scoped_rule_target")
        .rows
        .is_empty());

    let cte = engine
        .sql(
            "CREATE RULE cte_rule AS ON INSERT TO scoped_rule_event DO ALSO WITH item AS (SELECT NEW.id AS id) INSERT INTO scoped_rule_target SELECT id FROM item",
            &[],
        )
        .expect_err("WITH queries cannot reference the event relation");
    assert_eq!(cte.sqlstate(), Some("0A000"));

    let conflict = engine
        .sql(
            "CREATE RULE conflict_rule AS ON INSERT TO scoped_rule_event DO ALSO INSERT INTO scoped_rule_target VALUES (NEW.id) ON CONFLICT (id) DO UPDATE SET id = NEW.id",
            &[],
        )
        .expect_err("ON CONFLICT DO UPDATE cannot reference the event relation");
    assert_eq!(conflict.sqlstate(), Some("42P01"));

    exec(
        &engine,
        "CREATE VIEW scoped_rule_view AS SELECT scoped_rule_target.id FROM scoped_rule_target",
    );
    exec(
        &engine,
        "CREATE RULE view_action_rule AS ON INSERT TO scoped_rule_event DO ALSO INSERT INTO scoped_rule_view VALUES (NEW.id)",
    );
}

#[test]
fn rule_returning_contract_is_validated_when_the_rule_is_created() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE returning_event (id INTEGER, note VARCHAR(3))",
    );
    exec(
        &engine,
        "CREATE TABLE returning_action (id BIGINT, note VARCHAR(20))",
    );
    let wrong_type = engine
        .sql(
            "CREATE RULE wrong_type AS ON INSERT TO returning_event DO INSTEAD INSERT INTO returning_action VALUES (NEW.id, NEW.note) RETURNING id, note::VARCHAR(3)",
            &[],
        )
        .expect_err("provider type must match the event row type");
    assert_eq!(wrong_type.sqlstate(), Some("42P17"));
    let wrong_size = engine
        .sql(
            "CREATE RULE wrong_size AS ON INSERT TO returning_event DO INSTEAD INSERT INTO returning_action VALUES (NEW.id, NEW.note) RETURNING id::INTEGER, note",
            &[],
        )
        .expect_err("provider type modifier must match the event row type");
    assert_eq!(wrong_size.sqlstate(), Some("42P17"));
    let too_few = engine
        .sql(
            "CREATE RULE too_few AS ON INSERT TO returning_event DO INSTEAD INSERT INTO returning_action VALUES (NEW.id, NEW.note) RETURNING id::INTEGER",
            &[],
        )
        .expect_err("provider must return the complete event row type");
    assert_eq!(too_few.sqlstate(), Some("42P17"));
    let conditional = engine
        .sql(
            "CREATE RULE conditional_returning AS ON INSERT TO returning_event WHERE NEW.id > 0 DO INSTEAD INSERT INTO returning_action VALUES (NEW.id, NEW.note) RETURNING id::INTEGER, note::VARCHAR(3)",
            &[],
        )
        .expect_err("conditional rules cannot provide RETURNING");
    assert_eq!(conditional.sqlstate(), Some("0A000"));
    let also = engine
        .sql(
            "CREATE RULE also_returning AS ON INSERT TO returning_event DO ALSO INSERT INTO returning_action VALUES (NEW.id, NEW.note) RETURNING id::INTEGER, note::VARCHAR(3)",
            &[],
        )
        .expect_err("non-INSTEAD rules cannot provide RETURNING");
    assert_eq!(also.sqlstate(), Some("0A000"));
    let multiple = engine
        .sql(
            "CREATE RULE multiple_returning AS ON INSERT TO returning_event DO INSTEAD (INSERT INTO returning_action VALUES (NEW.id, NEW.note) RETURNING id::INTEGER, note::VARCHAR(3); INSERT INTO returning_action VALUES (NEW.id + 1, NEW.note) RETURNING id::INTEGER, note::VARCHAR(3);)",
            &[],
        )
        .expect_err("one rule cannot have multiple RETURNING providers");
    assert_eq!(multiple.sqlstate(), Some("0A000"));
    let insert_event_reference = engine
        .sql(
            "CREATE RULE insert_event_reference AS ON INSERT TO returning_event DO INSTEAD INSERT INTO returning_action VALUES (NEW.id, NEW.note) RETURNING NEW.id, note::VARCHAR(3)",
            &[],
        )
        .expect_err("INSERT action NEW resolves against its action target first");
    assert_eq!(insert_event_reference.sqlstate(), Some("42P17"));
    exec(
        &engine,
        "CREATE TABLE returning_action_without_id (value INTEGER, note VARCHAR(3))",
    );
    let invisible_insert_event = engine
        .sql(
            "CREATE RULE invisible_insert_event AS ON INSERT TO returning_event DO INSTEAD INSERT INTO returning_action_without_id VALUES (NEW.id, NEW.note) RETURNING NEW.id, note",
            &[],
        )
        .expect_err("INSERT action RETURNING cannot fall back to the rule event row");
    assert_eq!(invisible_insert_event.sqlstate(), Some("42703"));
}

#[test]
fn persistent_rule_catalog_restoration_does_not_reenter_transaction_snapshots() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("rule-catalog-restoration.db");
    let engine = Engine::open(&database).unwrap();
    exec(&engine, "CREATE TABLE view_action_base (id INTEGER)");
    exec(
        &engine,
        "CREATE VIEW view_action_target AS SELECT id FROM view_action_base",
    );
    exec(&engine, "CREATE TABLE view_action_event (id INTEGER)");
    exec(
        &engine,
        "CREATE RULE view_action_rule AS ON INSERT TO view_action_event
         DO ALSO INSERT INTO view_action_target VALUES (NEW.id)",
    );
    exec(
        &engine,
        "CREATE TABLE returning_validation_event (id INTEGER, note VARCHAR(3))",
    );
    exec(
        &engine,
        "CREATE TABLE returning_validation_action (id BIGINT, note VARCHAR(20))",
    );

    let error = engine
        .sql(
            "CREATE RULE returning_wrong_type AS ON INSERT TO returning_validation_event
             DO INSTEAD INSERT INTO returning_validation_action VALUES (NEW.id, NEW.note)
             RETURNING id, note::VARCHAR(3)",
            &[],
        )
        .expect_err("an incompatible rule RETURNING type must be rejected");
    assert_eq!(error.sqlstate(), Some("42P17"));
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS n FROM pg_rewrite WHERE rulename = 'view_action_rule'",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(1))
    );

    exec(&engine, "BEGIN");
    exec(&engine, "CREATE TABLE rolled_back_rule_marker (id INTEGER)");
    exec(&engine, "ROLLBACK");
    let missing = engine
        .sql("SELECT * FROM rolled_back_rule_marker", &[])
        .expect_err("explicit rollback must restore the persistent catalog");
    assert_eq!(missing.sqlstate(), Some("42P01"));
}

#[test]
fn insert_rule_returning_maps_provider_rows_to_the_event_relation() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE insert_returning_event (z INTEGER, a TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE insert_returning_action (mapped_z INTEGER, mapped_a TEXT)",
    );
    exec(
        &engine,
        "CREATE RULE insert_returning_provider AS ON INSERT TO insert_returning_event DO INSTEAD INSERT INTO insert_returning_action VALUES (NEW.z, NEW.a) RETURNING mapped_z + 10, mapped_a || '!'",
    );

    let result = exec(
        &engine,
        "INSERT INTO insert_returning_event VALUES (1, 'one'), (2, 'two') RETURNING old.z AS old_z, new.z AS new_z, z * 2 AS doubled, a",
    );
    assert_eq!(result.affected_rows, 2);
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.value_at(0, 0), Some(&Value::Null));
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(11)));
    assert_eq!(result.value_at(0, 2), Some(&Value::Int(22)));
    assert_eq!(result.value_at(0, 3), Some(&Value::Str("one!".into())));
    assert_eq!(result.value_at(1, 1), Some(&Value::Int(12)));
    assert_eq!(result.value_at(1, 2), Some(&Value::Int(24)));
    assert_eq!(result.value_at(1, 3), Some(&Value::Str("two!".into())));
    assert!(exec(&engine, "SELECT * FROM insert_returning_event")
        .rows
        .is_empty());
    assert_eq!(
        exec(
            &engine,
            "SELECT mapped_z FROM insert_returning_action ORDER BY mapped_z"
        )
        .rows
        .iter()
        .map(|row| row.get("mapped_z"))
        .collect::<Vec<_>>(),
        [Some(&Value::Int(1)), Some(&Value::Int(2))]
    );
}

#[test]
fn update_and_delete_rule_returning_preserve_action_old_and_new_images() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE update_returning_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE update_returning_action (id INTEGER PRIMARY KEY, mapped INTEGER)",
    );
    exec(&engine, "INSERT INTO update_returning_event VALUES (1, 10)");
    exec(
        &engine,
        "INSERT INTO update_returning_action VALUES (1, 100)",
    );
    exec(
        &engine,
        "CREATE RULE update_returning_provider AS ON UPDATE TO update_returning_event DO INSTEAD UPDATE update_returning_action SET mapped = NEW.value + 10 WHERE id = OLD.id RETURNING id, mapped + 100",
    );

    let updated = exec(
        &engine,
        "UPDATE update_returning_event SET value = value + 1 RETURNING old.value AS old_value, new.value AS new_value, value",
    );
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(updated.value_at(0, 0), Some(&Value::Int(200)));
    assert_eq!(updated.value_at(0, 1), Some(&Value::Int(121)));
    assert_eq!(updated.value_at(0, 2), Some(&Value::Int(121)));
    assert_eq!(
        exec(&engine, "SELECT value FROM update_returning_event").value_at(0, 0),
        Some(&Value::Int(10))
    );
    assert_eq!(
        exec(&engine, "SELECT mapped FROM update_returning_action").value_at(0, 0),
        Some(&Value::Int(21))
    );

    exec(
        &engine,
        "CREATE TABLE delete_returning_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE delete_returning_action (id INTEGER PRIMARY KEY, mapped INTEGER)",
    );
    exec(&engine, "INSERT INTO delete_returning_event VALUES (1, 10)");
    exec(
        &engine,
        "INSERT INTO delete_returning_action VALUES (1, 100)",
    );
    exec(
        &engine,
        "CREATE RULE delete_returning_provider AS ON DELETE TO delete_returning_event DO INSTEAD DELETE FROM delete_returning_action WHERE id = OLD.id RETURNING id, mapped + 10",
    );
    let deleted = exec(
        &engine,
        "DELETE FROM delete_returning_event RETURNING old.value AS old_value, new.value AS new_value, value",
    );
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(deleted.value_at(0, 0), Some(&Value::Int(110)));
    assert_eq!(deleted.value_at(0, 1), Some(&Value::Null));
    assert_eq!(deleted.value_at(0, 2), Some(&Value::Int(110)));
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS count FROM delete_returning_event"
        )
        .value_at(0, 0),
        Some(&Value::Int(1))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS count FROM delete_returning_action"
        )
        .value_at(0, 0),
        Some(&Value::Int(0))
    );
}

#[test]
fn rule_returning_preserves_update_from_and_delete_using_context() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE update_context_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE update_context_source (id INTEGER PRIMARY KEY, delta INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE update_context_action (id INTEGER PRIMARY KEY, mapped INTEGER)",
    );
    exec(&engine, "INSERT INTO update_context_event VALUES (1, 10)");
    exec(&engine, "INSERT INTO update_context_source VALUES (1, 5)");
    exec(&engine, "INSERT INTO update_context_action VALUES (1, 100)");
    exec(
        &engine,
        "CREATE RULE update_context_provider AS ON UPDATE TO update_context_event DO INSTEAD UPDATE update_context_action SET mapped = NEW.value WHERE id = OLD.id RETURNING id, mapped",
    );

    let updated = exec(
        &engine,
        "UPDATE update_context_event AS event SET value = event.value + source.delta FROM update_context_source AS source WHERE event.id = source.id RETURNING source.delta, old.value, new.value",
    );
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(updated.value_at(0, 0), Some(&Value::Int(5)));
    assert_eq!(updated.value_at(0, 1), Some(&Value::Int(100)));
    assert_eq!(updated.value_at(0, 2), Some(&Value::Int(15)));
    assert_eq!(
        exec(&engine, "SELECT value FROM update_context_event").value_at(0, 0),
        Some(&Value::Int(10))
    );
    assert_eq!(
        exec(&engine, "SELECT mapped FROM update_context_action").value_at(0, 0),
        Some(&Value::Int(15))
    );

    exec(
        &engine,
        "CREATE TABLE delete_context_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE delete_context_source (id INTEGER PRIMARY KEY, tag TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE delete_context_action (id INTEGER PRIMARY KEY, mapped INTEGER)",
    );
    exec(&engine, "INSERT INTO delete_context_event VALUES (1, 10)");
    exec(
        &engine,
        "INSERT INTO delete_context_source VALUES (1, 'hit')",
    );
    exec(&engine, "INSERT INTO delete_context_action VALUES (1, 100)");
    exec(
        &engine,
        "CREATE RULE delete_context_provider AS ON DELETE TO delete_context_event DO INSTEAD DELETE FROM delete_context_action WHERE id = OLD.id RETURNING id, mapped",
    );

    let deleted = exec(
        &engine,
        "DELETE FROM delete_context_event AS event USING delete_context_source AS source WHERE event.id = source.id RETURNING source.tag, old.value, new.value",
    );
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(deleted.value_at(0, 0), Some(&Value::Str("hit".into())));
    assert_eq!(deleted.value_at(0, 1), Some(&Value::Int(100)));
    assert_eq!(deleted.value_at(0, 2), Some(&Value::Null));
    assert_eq!(
        exec(&engine, "SELECT value FROM delete_context_event").value_at(0, 0),
        Some(&Value::Int(10))
    );
    assert!(exec(&engine, "SELECT * FROM delete_context_action")
        .rows
        .is_empty());
}

#[test]
fn rule_returning_retargets_explicit_action_image_aliases() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE alias_returning_event (id INTEGER PRIMARY KEY, value INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE alias_returning_action (id INTEGER PRIMARY KEY, mapped INTEGER)",
    );
    exec(&engine, "INSERT INTO alias_returning_event VALUES (1, 10)");
    exec(
        &engine,
        "INSERT INTO alias_returning_action VALUES (1, 100)",
    );
    exec(
        &engine,
        "CREATE RULE alias_returning_provider AS ON UPDATE TO alias_returning_event DO INSTEAD UPDATE alias_returning_action SET mapped = NEW.value + 10 WHERE id = OLD.id RETURNING WITH (OLD AS action_old, NEW AS action_new) id, action_old.mapped + action_new.mapped",
    );
    let result = exec(
        &engine,
        "UPDATE alias_returning_event SET value = value + 1 RETURNING old.value AS old_value, new.value AS new_value, value",
    );
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(200)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(42)));
    assert_eq!(result.value_at(0, 2), Some(&Value::Int(121)));
}

#[test]
fn rule_returning_requires_one_active_provider_only_when_instead_can_suppress() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE returning_source (id INTEGER)");
    exec(&engine, "CREATE TABLE returning_log (id INTEGER)");
    exec(
        &engine,
        "CREATE RULE returning_also AS ON INSERT TO returning_source DO ALSO INSERT INTO returning_log VALUES (NEW.id)",
    );
    let ordinary = exec(
        &engine,
        "INSERT INTO returning_source VALUES (1) RETURNING id",
    );
    assert_eq!(ordinary.value_at(0, 0), Some(&Value::Int(1)));

    exec(
        &engine,
        "CREATE RULE returning_conditional_suppress AS ON INSERT TO returning_source WHERE NEW.id < 0 DO INSTEAD NOTHING",
    );
    let missing = engine
        .sql("INSERT INTO returning_source VALUES (2) RETURNING id", &[])
        .expect_err("an INSTEAD rule requires one unconditional provider");
    assert_eq!(missing.sqlstate(), Some("0A000"));
    assert_eq!(
        exec(&engine, "SELECT id FROM returning_source ORDER BY id")
            .rows
            .len(),
        1
    );

    exec(&engine, "CREATE TABLE returning_action_a (id INTEGER)");
    exec(&engine, "CREATE TABLE returning_action_b (id INTEGER)");
    exec(
        &engine,
        "CREATE RULE returning_provider_a AS ON INSERT TO returning_source DO INSTEAD INSERT INTO returning_action_a VALUES (NEW.id) RETURNING id",
    );
    exec(
        &engine,
        "CREATE RULE returning_provider_b AS ON INSERT TO returning_source DO INSTEAD INSERT INTO returning_action_b VALUES (NEW.id) RETURNING id",
    );
    let multiple = engine
        .sql("INSERT INTO returning_source VALUES (3) RETURNING id", &[])
        .expect_err("multiple active providers must fail before action execution");
    assert_eq!(multiple.sqlstate(), Some("0A000"));
    assert!(exec(&engine, "SELECT * FROM returning_action_a")
        .rows
        .is_empty());
    assert!(exec(&engine, "SELECT * FROM returning_action_b")
        .rows
        .is_empty());
}

#[test]
fn recursive_rules_and_rule_incompatible_dml_fail_atomically() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE recursive_items (id INTEGER PRIMARY KEY)",
    );
    exec(
        &engine,
        "CREATE RULE recursive_insert AS ON INSERT TO public.recursive_items DO ALSO INSERT INTO recursive_items VALUES (NEW.id + 100)",
    );
    let recursive = engine
        .sql("INSERT INTO public.recursive_items VALUES (1)", &[])
        .expect_err("recursive rule must fail");
    assert_eq!(recursive.sqlstate(), Some("42P17"));
    assert!(recursive
        .to_string()
        .contains("infinite recursion detected in rules for relation \"recursive_items\""));
    assert!(exec(&engine, "SELECT id FROM recursive_items")
        .rows
        .is_empty());

    let conflict = engine
        .sql(
            "INSERT INTO recursive_items VALUES (1) ON CONFLICT DO NOTHING",
            &[],
        )
        .expect_err("ON CONFLICT with an active INSERT rule must fail");
    assert_eq!(conflict.sqlstate(), Some("0A000"));
    let merge = engine
        .sql(
            "MERGE INTO recursive_items AS target USING (VALUES (1)) AS source(id)
             ON target.id = source.id
             WHEN NOT MATCHED THEN INSERT VALUES (source.id)",
            &[],
        )
        .expect_err("MERGE with active rules must fail");
    assert_eq!(merge.sqlstate(), Some("0A000"));

    exec(
        &engine,
        "ALTER TABLE recursive_items DISABLE RULE recursive_insert",
    );
    exec(
        &engine,
        "INSERT INTO recursive_items VALUES (1) ON CONFLICT DO NOTHING",
    );
    exec(
        &engine,
        "MERGE INTO recursive_items AS target USING (VALUES (2)) AS source(id)
         ON target.id = source.id
         WHEN NOT MATCHED THEN INSERT VALUES (source.id)",
    );
    assert_eq!(
        exec(&engine, "SELECT id FROM recursive_items").rows.len(),
        2
    );
}

#[test]
fn rule_column_dependencies_follow_rename_restrict_and_cascade() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rule_items (id INTEGER PRIMARY KEY, value INTEGER, disposable INTEGER)",
    );
    exec(&engine, "CREATE TABLE rule_log (value INTEGER)");
    exec(
        &engine,
        "CREATE RULE value_rule AS ON UPDATE TO rule_items WHERE NEW.value > OLD.value DO ALSO INSERT INTO rule_log VALUES (NEW.value)",
    );
    exec(
        &engine,
        "CREATE RULE disposable_rule AS ON UPDATE TO rule_items DO ALSO INSERT INTO rule_log VALUES (NEW.disposable)",
    );
    exec(
        &engine,
        "ALTER TABLE rule_items RENAME COLUMN value TO amount",
    );
    exec(&engine, "INSERT INTO rule_items VALUES (1, 10, 7)");
    exec(&engine, "UPDATE rule_items SET amount = 11");
    assert_eq!(
        exec(&engine, "SELECT value FROM rule_log ORDER BY value")
            .rows
            .iter()
            .map(|row| row.get("value"))
            .collect::<Vec<_>>(),
        [Some(&Value::Int(7)), Some(&Value::Int(11))]
    );
    let restrict = engine
        .sql("ALTER TABLE rule_items DROP COLUMN disposable", &[])
        .expect_err("dependent rule must restrict column drop");
    assert_eq!(restrict.sqlstate(), Some("2BP01"));
    exec(
        &engine,
        "ALTER TABLE rule_items DROP COLUMN disposable CASCADE",
    );
    assert!(exec(
        &engine,
        "SELECT oid FROM pg_rewrite WHERE rulename = 'disposable_rule'",
    )
    .rows
    .is_empty());
}

#[test]
fn rule_column_rename_deparse_only_rewrites_exact_row_identifiers() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE deparse_items (id INTEGER PRIMARY KEY, value INTEGER, value_suffix INTEGER)",
    );
    exec(&engine, "CREATE TABLE deparse_log (message TEXT)");
    exec(
        &engine,
        "CREATE RULE deparse_rule AS ON UPDATE TO deparse_items DO ALSO INSERT INTO deparse_log VALUES ('NEW.value:' || NEW.value || ':' || NEW.value_suffix)",
    );
    exec(
        &engine,
        "ALTER TABLE deparse_items RENAME COLUMN value TO amount",
    );
    let definition = exec(
        &engine,
        "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'deparse_rule'",
    );
    let Some(Value::Str(definition)) = definition.rows[0].get("definition") else {
        panic!("expected rule definition text");
    };
    assert!(definition.contains("'NEW.value:'"));
    assert!(definition.contains("new.amount"));
    assert!(definition.contains("new.value_suffix"));
    assert!(!definition.contains("new.amount_suffix"));
}

#[test]
fn rule_catalog_enable_rename_drop_and_reopen_are_durable() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rules.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(&engine, "CREATE TABLE rule_items (id INTEGER PRIMARY KEY)");
        exec(&engine, "CREATE TABLE rule_log (id INTEGER)");
        exec(
            &engine,
            "CREATE RULE catalog_rule AS ON INSERT TO rule_items DO ALSO INSERT INTO rule_log VALUES (NEW.id)",
        );
        exec(&engine, "ALTER TABLE rule_items DISABLE RULE catalog_rule");
        exec(
            &engine,
            "CREATE OR REPLACE RULE catalog_rule AS ON INSERT TO rule_items DO ALSO INSERT INTO rule_log VALUES (NEW.id + 10)",
        );
    }
    let engine = Engine::open(&path).unwrap();
    let catalog = exec(
        &engine,
        "SELECT rulename, ev_type, ev_enabled, is_instead FROM pg_rewrite WHERE rulename = 'catalog_rule'",
    );
    assert_eq!(catalog.rows.len(), 1);
    assert_eq!(
        catalog.rows[0].get("ev_type"),
        Some(&Value::Str("3".into()))
    );
    assert_eq!(
        catalog.rows[0].get("ev_enabled"),
        Some(&Value::Str("D".into()))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT relhasrules FROM pg_class WHERE relname = 'rule_items'",
        )
        .rows[0]
            .get("relhasrules"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT hasrules FROM pg_tables WHERE tablename = 'rule_items'",
        )
        .rows[0]
            .get("hasrules"),
        Some(&Value::Bool(true))
    );
    exec(&engine, "INSERT INTO rule_items VALUES (1)");
    assert!(exec(&engine, "SELECT id FROM rule_log").rows.is_empty());
    exec(
        &engine,
        "ALTER RULE catalog_rule ON rule_items RENAME TO renamed_rule",
    );
    exec(&engine, "ALTER TABLE rule_items ENABLE RULE renamed_rule");
    exec(&engine, "INSERT INTO rule_items VALUES (2)");
    assert_eq!(
        exec(&engine, "SELECT id FROM rule_log").rows[0].get("id"),
        Some(&Value::Int(12))
    );
    let definition = exec(
        &engine,
        "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'renamed_rule'",
    );
    assert!(
        matches!(definition.rows[0].get("definition"), Some(Value::Str(value)) if value.contains("CREATE RULE renamed_rule AS ON INSERT TO rule_items DO ALSO") && value.contains("new.id + 10"))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT pg_catalog.pg_get_ruledef(r.oid, true) LIKE 'CREATE RULE renamed_rule%' AS has_definition FROM pg_catalog.pg_rewrite AS r JOIN pg_catalog.pg_class AS c ON c.oid = r.ev_class WHERE c.relname = 'rule_items' AND r.rulename = 'renamed_rule'",
        )
        .rows[0]
            .get("has_definition"),
        Some(&Value::Bool(true))
    );
    let pg_rules = exec(
        &engine,
        "SELECT schemaname, tablename, rulename, definition LIKE 'CREATE RULE renamed_rule%' AS has_definition FROM pg_catalog.pg_rules WHERE tablename = 'rule_items' AND rulename = 'renamed_rule'",
    );
    assert_eq!(pg_rules.rows.len(), 1);
    assert_eq!(
        pg_rules.rows[0].get("schemaname"),
        Some(&Value::Str("public".into()))
    );
    assert_eq!(
        pg_rules.rows[0].get("has_definition"),
        Some(&Value::Bool(true))
    );
    exec(&engine, "DROP RULE renamed_rule ON rule_items");
    assert!(exec(
        &engine,
        "SELECT oid FROM pg_rewrite WHERE rulename = 'renamed_rule'"
    )
    .rows
    .is_empty());
}

#[test]
fn returning_rule_action_targets_restore_without_session_search_path() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("returning-rules.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(&engine, "CREATE SCHEMA rule_scope");
        exec(&engine, "SET search_path = rule_scope, public");
        exec(&engine, "CREATE TABLE event_rows (id INTEGER)");
        exec(&engine, "CREATE TABLE action_rows (id INTEGER)");
        exec(
            &engine,
            "CREATE RULE returning_provider AS ON INSERT TO event_rows DO INSTEAD INSERT INTO action_rows VALUES (NEW.id) RETURNING id",
        );
    }
    let engine = Engine::open(&path).expect("qualified rule action target must restore");
    exec(&engine, "SET search_path = rule_scope, public");
    let result = exec(&engine, "INSERT INTO event_rows VALUES (7) RETURNING id");
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(7)));
    assert_eq!(
        exec(&engine, "SELECT id FROM action_rows").value_at(0, 0),
        Some(&Value::Int(7))
    );
}

#[test]
fn rule_action_targets_follow_search_path_before_public_views() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE public.rule_path_event (id INTEGER)");
    exec(
        &engine,
        "CREATE TABLE public.rule_path_view_base (id INTEGER)",
    );
    exec(
        &engine,
        "CREATE VIEW public.rule_path_target AS SELECT id FROM public.rule_path_view_base",
    );
    exec(&engine, "CREATE SCHEMA rule_path_first");
    exec(
        &engine,
        "CREATE TABLE rule_path_first.rule_path_target (id INTEGER)",
    );
    exec(&engine, "SET search_path = rule_path_first, public");
    exec(
        &engine,
        "CREATE RULE rule_path_action AS ON INSERT TO public.rule_path_event DO ALSO INSERT INTO rule_path_target VALUES (NEW.id)",
    );

    exec(&engine, "INSERT INTO public.rule_path_event VALUES (7)");

    assert_eq!(
        exec(&engine, "SELECT id FROM rule_path_first.rule_path_target").value_at(0, 0),
        Some(&Value::Int(7))
    );
    assert!(exec(&engine, "SELECT id FROM public.rule_path_view_base")
        .rows
        .is_empty());
}

#[test]
fn select_return_rule_replaces_view_and_cannot_be_dropped() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE rule_base (value INTEGER)");
    exec(&engine, "INSERT INTO rule_base VALUES (1)");
    let reserved_return = engine
        .sql(
            "CREATE RULE \"_RETURN\" AS ON INSERT TO rule_base DO INSTEAD NOTHING",
            &[],
        )
        .expect_err("_RETURN is reserved for ON SELECT view rules");
    assert_eq!(reserved_return.sqlstate(), Some("42P17"));
    exec(
        &engine,
        "CREATE MATERIALIZED VIEW rule_snapshot AS SELECT value FROM rule_base",
    );
    let materialized_rule = engine
        .sql(
            "CREATE RULE snapshot_update AS ON UPDATE TO rule_snapshot DO INSTEAD NOTHING",
            &[],
        )
        .expect_err("materialized views cannot have rules");
    assert_eq!(materialized_rule.sqlstate(), Some("0A000"));
    exec(
        &engine,
        "CREATE VIEW rule_view AS SELECT value FROM rule_base",
    );
    exec(
        &engine,
        "CREATE OR REPLACE RULE \"_RETURN\" AS ON SELECT TO rule_view DO INSTEAD SELECT value + 1 AS value FROM rule_base",
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM rule_view").rows[0].get("value"),
        Some(&Value::Int(2))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) AS n FROM pg_rewrite WHERE ev_class = 'rule_view'::regclass AND rulename = '_RETURN'",
        )
        .rows[0]
            .get("n"),
        Some(&Value::Int(1))
    );
    let rename_return = engine
        .sql(
            "ALTER RULE \"_RETURN\" ON rule_view RENAME TO renamed_return",
            &[],
        )
        .expect_err("view return rule cannot be renamed");
    assert_eq!(rename_return.sqlstate(), Some("42P17"));
    let disable_return = engine
        .sql("ALTER TABLE rule_view DISABLE RULE \"_RETURN\"", &[])
        .expect_err("ALTER TABLE rule enable modes are not supported for views");
    assert_eq!(disable_return.sqlstate(), Some("42809"));
    let drop_error = engine
        .sql("DROP RULE \"_RETURN\" ON rule_view", &[])
        .expect_err("view return rule must be required");
    assert_eq!(drop_error.sqlstate(), Some("2BP01"));
}
