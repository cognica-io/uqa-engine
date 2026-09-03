//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 rewrite-rule execution and catalog lifecycle coverage.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

#[path = "sql_rules/persistence.rs"]
mod persistence;
#[path = "sql_rules/privilege_subjects.rs"]
mod privilege_subjects;
#[path = "sql_rules/privileges.rs"]
mod privileges;
#[path = "sql_rules/returning.rs"]
mod returning;

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
fn insert_default_values_rule_actions_preserve_event_cardinality() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE default_rule_events(id INTEGER); CREATE TABLE default_rule_empty(id INTEGER); CREATE TABLE default_rule_log(id BIGSERIAL PRIMARY KEY, payload INTEGER DEFAULT 9)",
    );
    exec(
        &engine,
        "CREATE RULE default_rule AS ON INSERT TO default_rule_events DO ALSO INSERT INTO default_rule_log DEFAULT VALUES",
    );

    exec(&engine, "INSERT INTO default_rule_events VALUES (1), (2)");
    exec(
        &engine,
        "INSERT INTO default_rule_events SELECT id FROM default_rule_empty",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT id || ':' || payload AS value FROM default_rule_log ORDER BY id",
        )
        .rows
        .iter()
        .map(|row| row.get("value"))
        .collect::<Vec<_>>(),
        [
            Some(&Value::Str("1:9".into())),
            Some(&Value::Str("2:9".into())),
        ]
    );
}

#[test]
fn session_replication_role_selects_rule_enable_modes() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE replication_rule_items (id INTEGER PRIMARY KEY); CREATE TABLE replication_rule_log (seq BIGSERIAL PRIMARY KEY, message TEXT)",
    );
    for rule in [
        "CREATE RULE rule_origin AS ON INSERT TO replication_rule_items DO ALSO INSERT INTO replication_rule_log(message) VALUES ('rule_origin:' || NEW.id::text)",
        "CREATE RULE rule_replica AS ON INSERT TO replication_rule_items DO ALSO INSERT INTO replication_rule_log(message) VALUES ('rule_replica:' || NEW.id::text)",
        "CREATE RULE rule_always AS ON INSERT TO replication_rule_items DO ALSO INSERT INTO replication_rule_log(message) VALUES ('rule_always:' || NEW.id::text)",
        "CREATE RULE rule_disabled AS ON INSERT TO replication_rule_items DO ALSO INSERT INTO replication_rule_log(message) VALUES ('rule_disabled:' || NEW.id::text)",
    ] {
        exec(&engine, rule);
    }
    exec(
        &engine,
        "ALTER TABLE replication_rule_items ENABLE REPLICA RULE rule_replica; ALTER TABLE replication_rule_items ENABLE ALWAYS RULE rule_always; ALTER TABLE replication_rule_items DISABLE RULE rule_disabled",
    );

    exec(
        &engine,
        "SET session_replication_role = origin; INSERT INTO replication_rule_items VALUES (1); SET session_replication_role = local; INSERT INTO replication_rule_items VALUES (2); SET session_replication_role = replica; INSERT INTO replication_rule_items VALUES (3); RESET session_replication_role",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT message FROM replication_rule_log ORDER BY seq",
            "message",
        ),
        [
            "rule_always:1",
            "rule_origin:1",
            "rule_always:2",
            "rule_origin:2",
            "rule_always:3",
            "rule_replica:3",
        ]
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
fn insert_select_and_constant_actions_follow_event_cardinality() {
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
fn empty_event_sets_preserve_rule_action_statement_semantics() {
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
    exec(
        &engine,
        "CREATE RULE c_empty_false AS ON UPDATE TO empty_rule_event WHERE false DO ALSO INSERT INTO empty_rule_constant VALUES ('false')",
    );
    exec(
        &engine,
        "CREATE RULE d_empty_true AS ON UPDATE TO empty_rule_event WHERE true DO ALSO INSERT INTO empty_rule_constant VALUES ('true')",
    );

    let result = exec(&engine, "UPDATE empty_rule_event SET value = value + 1");

    assert_eq!(result.affected_rows, 0);
    assert_eq!(
        strings(
            &engine,
            "SELECT value FROM empty_rule_constant ORDER BY value",
            "value"
        ),
        ["ran", "true"]
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
