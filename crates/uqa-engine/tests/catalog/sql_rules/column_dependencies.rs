//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn rule_action_select_can_correlate_event_rows_with_multi_column_source() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE action_events(id INTEGER);
         CREATE TABLE action_source(source_value INTEGER, predicate_value INTEGER);
         CREATE TABLE action_log(target_value INTEGER);
         INSERT INTO action_source VALUES (10, 1);
         CREATE RULE action_columns AS ON INSERT TO action_events DO ALSO
           INSERT INTO action_log(target_value)
             SELECT source.source_value FROM action_source AS source
             WHERE source.predicate_value = NEW.id;
         INSERT INTO action_events VALUES (1)",
    );
    assert_eq!(
        exec(&engine, "SELECT target_value FROM action_log").rows[0].get("target_value"),
        Some(&Value::Int(10))
    );
}

#[test]
fn rule_source_and_action_target_columns_follow_rename_and_reopen() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rule-column-dependencies.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE dependency_events(id INTEGER);
             CREATE TABLE dependency_source(source_value INTEGER, predicate_value INTEGER, disposable_value INTEGER);
             CREATE TABLE dependency_log(target_value INTEGER);
             INSERT INTO dependency_source VALUES (10, 1, 100);
             CREATE RULE bound_columns AS ON INSERT TO dependency_events
               WHERE EXISTS (
                 SELECT 1 FROM dependency_source AS source
                 WHERE source.predicate_value = NEW.id
               )
               DO ALSO INSERT INTO dependency_log(target_value)
                 SELECT source.source_value FROM dependency_source AS source
                 WHERE source.predicate_value = NEW.id;
             ALTER TABLE dependency_source RENAME COLUMN source_value TO renamed_source_value;
             ALTER TABLE dependency_source RENAME COLUMN predicate_value TO renamed_predicate_value;
             ALTER TABLE dependency_log RENAME COLUMN target_value TO renamed_target_value;
             ALTER TABLE dependency_source DROP COLUMN disposable_value;
             INSERT INTO dependency_events VALUES (1)",
        );
        assert_eq!(
            exec(&engine, "SELECT renamed_target_value FROM dependency_log").rows[0]
                .get("renamed_target_value"),
            Some(&Value::Int(10))
        );
        let definition = exec(
            &engine,
            "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'bound_columns'",
        );
        let Some(Value::Str(definition)) = definition.rows[0].get("definition") else {
            panic!("expected rule definition text");
        };
        assert!(
            definition.contains("source.renamed_source_value AS source_value"),
            "{definition}"
        );
        assert!(
            definition.contains("source.renamed_predicate_value"),
            "{definition}"
        );
        assert!(definition.contains("renamed_target_value"), "{definition}");
    }

    let engine = Engine::open(&path).expect("column-bound rule catalog must reopen");
    exec(&engine, "INSERT INTO dependency_events VALUES (1)");
    assert_eq!(
        exec(
            &engine,
            "SELECT renamed_target_value FROM dependency_log ORDER BY renamed_target_value",
        )
        .rows
        .iter()
        .map(|row| row.get("renamed_target_value"))
        .collect::<Vec<_>>(),
        [Some(&Value::Int(10)), Some(&Value::Int(10))]
    );

    for (table, column) in [
        ("dependency_source", "renamed_source_value"),
        ("dependency_source", "renamed_predicate_value"),
        ("dependency_log", "renamed_target_value"),
    ] {
        let error = engine
            .sql(&format!("ALTER TABLE {table} DROP COLUMN {column}"), &[])
            .expect_err("an exact rule column dependency must restrict DROP COLUMN");
        assert_eq!(error.sqlstate(), Some("2BP01"), "{table}.{column}: {error}");
    }
    exec(
        &engine,
        "ALTER TABLE dependency_source DROP COLUMN renamed_predicate_value CASCADE",
    );
    assert!(exec(
        &engine,
        "SELECT rulename FROM pg_rewrite WHERE rulename = 'bound_columns'",
    )
    .rows
    .is_empty());
}

#[test]
fn rule_update_source_and_target_columns_follow_exact_owners() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE update_dependency_events(id INTEGER);
         CREATE TABLE update_dependency_source(source_key INTEGER, source_value INTEGER, disposable_source INTEGER);
         CREATE TABLE update_dependency_target(target_key INTEGER, target_value INTEGER, disposable_target INTEGER);
         INSERT INTO update_dependency_source VALUES (1, 9, 100);
         INSERT INTO update_dependency_target VALUES (1, 0, 200);
         CREATE RULE update_bound_columns AS ON INSERT TO update_dependency_events DO ALSO
           UPDATE update_dependency_target AS target
           SET target_value = source.source_value
           FROM update_dependency_source AS source
           WHERE target.target_key = source.source_key AND source.source_key = NEW.id;
         ALTER TABLE update_dependency_source RENAME COLUMN source_key TO renamed_source_key;
         ALTER TABLE update_dependency_source RENAME COLUMN source_value TO renamed_source_value;
         ALTER TABLE update_dependency_target RENAME COLUMN target_key TO renamed_target_key;
         ALTER TABLE update_dependency_target RENAME COLUMN target_value TO renamed_target_value;
         ALTER TABLE update_dependency_source DROP COLUMN disposable_source;
         ALTER TABLE update_dependency_target DROP COLUMN disposable_target;
         INSERT INTO update_dependency_events VALUES (1)",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT renamed_target_value FROM update_dependency_target",
        )
        .value_at(0, 0),
        Some(&Value::Int(9))
    );
    for (table, column) in [
        ("update_dependency_source", "renamed_source_key"),
        ("update_dependency_source", "renamed_source_value"),
        ("update_dependency_target", "renamed_target_key"),
        ("update_dependency_target", "renamed_target_value"),
    ] {
        let error = engine
            .sql(&format!("ALTER TABLE {table} DROP COLUMN {column}"), &[])
            .expect_err("the bound rule column must restrict DROP COLUMN");
        assert_eq!(error.sqlstate(), Some("2BP01"), "{table}.{column}: {error}");
    }
}

#[test]
fn rule_column_lifecycle_rolls_back_with_the_table_change() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE rollback_dependency_events(id INTEGER);
         CREATE TABLE rollback_dependency_source(source_value INTEGER);
         CREATE TABLE rollback_dependency_log(target_value INTEGER);
         INSERT INTO rollback_dependency_source VALUES (12);
         CREATE RULE rollback_bound_column AS ON INSERT TO rollback_dependency_events DO ALSO
           INSERT INTO rollback_dependency_log(target_value)
           SELECT source.source_value FROM rollback_dependency_source AS source",
    );
    exec(
        &engine,
        "BEGIN;
         ALTER TABLE rollback_dependency_source RENAME COLUMN source_value TO renamed_source_value;
         ROLLBACK;
         INSERT INTO rollback_dependency_events VALUES (1)",
    );
    assert_eq!(
        exec(&engine, "SELECT target_value FROM rollback_dependency_log").value_at(0, 0),
        Some(&Value::Int(12))
    );
    exec(
        &engine,
        "BEGIN;
         ALTER TABLE rollback_dependency_source DROP COLUMN source_value CASCADE;
         ROLLBACK;
         INSERT INTO rollback_dependency_events VALUES (2)",
    );
    assert_eq!(
        exec(&engine, "SELECT count(*) FROM rollback_dependency_log",).value_at(0, 0),
        Some(&Value::Int(2))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) FROM pg_rewrite WHERE rulename = 'rollback_bound_column'",
        )
        .value_at(0, 0),
        Some(&Value::Int(1))
    );
}

#[test]
fn rule_unqualified_source_column_binding_does_not_drift_after_add_column() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE binding_events(id INTEGER);
         CREATE TABLE binding_left(id INTEGER, selected_value INTEGER);
         CREATE TABLE binding_right(id INTEGER);
         CREATE TABLE binding_log(value INTEGER);
         INSERT INTO binding_left VALUES (1, 11);
         INSERT INTO binding_right VALUES (1);
         CREATE RULE stable_unqualified_column AS ON INSERT TO binding_events DO ALSO
           INSERT INTO binding_log
             SELECT selected_value
             FROM binding_left AS left_source
             JOIN binding_right AS right_source ON left_source.id = right_source.id;
         ALTER TABLE binding_right ADD COLUMN selected_value INTEGER;
         UPDATE binding_right SET selected_value = 99;
         INSERT INTO binding_events VALUES (1)",
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM binding_log").rows[0].get("value"),
        Some(&Value::Int(11))
    );
}

#[test]
fn rule_join_keys_keep_their_visible_name_when_one_side_is_renamed() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rule-join-column-aliases.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE join_key_events(id INTEGER);
             CREATE TABLE join_key_left(disposable_value INTEGER, key_value INTEGER, left_value INTEGER);
             CREATE TABLE join_key_right(key_value INTEGER, right_value INTEGER);
             CREATE TABLE join_key_log(value INTEGER);
             INSERT INTO join_key_left VALUES (99, 1, 10);
             INSERT INTO join_key_right VALUES (1, 20);
             CREATE RULE join_key_rule AS ON INSERT TO join_key_events DO ALSO
               INSERT INTO join_key_log
               SELECT joined.key_value
             FROM (join_key_left AS left_source
               JOIN join_key_right AS right_source USING (key_value))
               AS joined(key_value, disposable_value, left_value, right_value)
             WHERE joined.key_value = NEW.id;
             ALTER TABLE join_key_left RENAME COLUMN key_value TO left_key",
        );
        exec(
            &engine,
            "BEGIN;
             ALTER TABLE join_key_left DROP COLUMN disposable_value;
             ROLLBACK",
        );
        exec(
            &engine,
            "ALTER TABLE join_key_left DROP COLUMN disposable_value;
             INSERT INTO join_key_events VALUES (1);
             ALTER TABLE join_key_right RENAME COLUMN key_value TO right_key;
             INSERT INTO join_key_events VALUES (1)",
        );
        assert_eq!(
            exec(&engine, "SELECT value FROM join_key_log ORDER BY value")
                .rows
                .iter()
                .map(|row| row.get("value"))
                .collect::<Vec<_>>(),
            [Some(&Value::Int(1)), Some(&Value::Int(1))]
        );
        let definition = exec(
            &engine,
            "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'join_key_rule'",
        );
        let Some(Value::Str(definition)) = definition.rows[0].get("definition") else {
            panic!("expected rule definition text");
        };
        assert!(
            definition.contains("join_key_left AS left_source(key_value, left_value)"),
            "{definition}"
        );
        assert!(
            definition.contains("join_key_right AS right_source(key_value, right_value)"),
            "{definition}"
        );
        assert!(
            definition.contains("AS joined(key_value, left_value, right_value)"),
            "{definition}"
        );
    }
    let engine = Engine::open(&path).expect("JOIN column aliases must survive catalog reopen");
    exec(&engine, "INSERT INTO join_key_events VALUES (1)");
    assert_eq!(
        exec(&engine, "SELECT count(*) FROM join_key_log").value_at(0, 0),
        Some(&Value::Int(3))
    );
    for (table, column) in [
        ("join_key_left", "left_key"),
        ("join_key_right", "right_key"),
    ] {
        let error = engine
            .sql(&format!("ALTER TABLE {table} DROP COLUMN {column}"), &[])
            .expect_err("a JOIN USING input must remain an exact dependency");
        assert_eq!(error.sqlstate(), Some("2BP01"), "{table}.{column}: {error}");
    }
}

#[test]
fn owned_sequence_cascade_does_not_restore_rules_while_rebinding_column_aliases() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE sequence_alias_owner(sequence_id BIGINT, retained_value BIGINT);
         CREATE SEQUENCE sequence_alias_ids OWNED BY sequence_alias_owner.sequence_id;
         CREATE TABLE sequence_alias_events(id INTEGER);
         CREATE TABLE sequence_alias_log(value BIGINT);
         CREATE TABLE sequence_dependency_events(id INTEGER);
         INSERT INTO sequence_alias_owner VALUES (1, 9);
         CREATE RULE retained_alias_rule AS ON INSERT TO sequence_alias_events DO ALSO
           INSERT INTO sequence_alias_log
           SELECT owner_alias.retained_alias
           FROM sequence_alias_owner AS owner_alias(sequence_alias, retained_alias);
         CREATE RULE owned_sequence_rule AS ON INSERT TO sequence_dependency_events DO ALSO
           INSERT INTO sequence_alias_log SELECT last_value FROM sequence_alias_ids;
         ALTER TABLE sequence_alias_owner DROP COLUMN sequence_id CASCADE;
         INSERT INTO sequence_alias_events VALUES (1)",
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM sequence_alias_log").value_at(0, 0),
        Some(&Value::Int(9))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) FROM pg_rewrite WHERE rulename = 'retained_alias_rule'",
        )
        .value_at(0, 0),
        Some(&Value::Int(1))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT count(*) FROM pg_rewrite WHERE rulename = 'owned_sequence_rule'",
        )
        .value_at(0, 0),
        Some(&Value::Int(0))
    );
    let definition = exec(
        &engine,
        "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'retained_alias_rule'",
    );
    let Some(Value::Str(definition)) = definition.rows[0].get("definition") else {
        panic!("expected retained rule definition text");
    };
    assert!(
        definition.contains("AS owner_alias(retained_alias)"),
        "{definition}"
    );
    assert!(
        !definition.contains("owner_alias(sequence_alias"),
        "{definition}"
    );
}

#[test]
fn join_range_aliases_preserve_only_explicit_positional_names() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE partial_join_events(id INTEGER);
         CREATE TABLE partial_join_left(key_value INTEGER, left_value INTEGER);
         CREATE TABLE partial_join_right(key_value INTEGER, right_value INTEGER);
         CREATE TABLE partial_join_log(value INTEGER);
         INSERT INTO partial_join_left VALUES (1, 7);
         INSERT INTO partial_join_right VALUES (1, 8);
         CREATE RULE partial_join_alias_rule AS ON INSERT TO partial_join_events DO ALSO
           INSERT INTO partial_join_log
           SELECT joined.left_value
           FROM (partial_join_left JOIN partial_join_right USING (key_value))
           AS joined(join_key)",
    );
    let initial = exec(
        &engine,
        "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'partial_join_alias_rule'",
    );
    let Some(Value::Str(initial)) = initial.rows[0].get("definition") else {
        panic!("expected initial rule definition text");
    };
    assert!(initial.contains("AS joined(join_key)"), "{initial}");
    assert!(!initial.contains("AS joined(join_key,"), "{initial}");

    exec(
        &engine,
        "ALTER TABLE partial_join_left RENAME COLUMN left_value TO renamed_left_value;
         INSERT INTO partial_join_events VALUES (1)",
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM partial_join_log").value_at(0, 0),
        Some(&Value::Int(7))
    );
    let renamed = exec(
        &engine,
        "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'partial_join_alias_rule'",
    );
    let Some(Value::Str(renamed)) = renamed.rows[0].get("definition") else {
        panic!("expected renamed rule definition text");
    };
    assert!(renamed.contains("AS joined(join_key)"), "{renamed}");
    assert!(!renamed.contains("AS joined(join_key,"), "{renamed}");
    assert!(
        renamed.contains("joined.renamed_left_value AS left_value"),
        "{renamed}"
    );
}

#[test]
fn subquery_outputs_follow_rename_without_synthesized_range_aliases() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE subquery_alias_events(id INTEGER);
         CREATE TABLE subquery_alias_source(source_value INTEGER);
         CREATE TABLE subquery_alias_log(value INTEGER);
         INSERT INTO subquery_alias_source VALUES (13);
         CREATE RULE subquery_alias_rule AS ON INSERT TO subquery_alias_events DO ALSO
           INSERT INTO subquery_alias_log
           SELECT nested.source_value
           FROM (
             SELECT source.source_value
             FROM subquery_alias_source AS source
           ) AS nested",
    );
    let initial = exec(
        &engine,
        "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'subquery_alias_rule'",
    );
    let Some(Value::Str(initial)) = initial.rows[0].get("definition") else {
        panic!("expected initial subquery rule definition text");
    };
    assert!(!initial.contains("AS nested(source_value)"), "{initial}");

    exec(
        &engine,
        "ALTER TABLE subquery_alias_source RENAME COLUMN source_value TO renamed_source_value;
         INSERT INTO subquery_alias_events VALUES (1)",
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM subquery_alias_log").value_at(0, 0),
        Some(&Value::Int(13))
    );
    let renamed = exec(
        &engine,
        "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'subquery_alias_rule'",
    );
    let Some(Value::Str(renamed)) = renamed.rows[0].get("definition") else {
        panic!("expected renamed subquery rule definition text");
    };
    assert!(!renamed.contains("AS nested(source_value)"), "{renamed}");
    assert!(
        renamed.contains("source.renamed_source_value AS source_value"),
        "{renamed}"
    );
}

#[test]
fn rule_natural_join_keys_keep_their_visible_name_after_rename() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE natural_key_events(id INTEGER);
         CREATE TABLE natural_key_left(key_value INTEGER, left_value INTEGER);
         CREATE TABLE natural_key_right(key_value INTEGER, right_value INTEGER);
         CREATE TABLE natural_key_log(value INTEGER);
         INSERT INTO natural_key_left VALUES (2, 10);
         INSERT INTO natural_key_right VALUES (2, 20);
         CREATE RULE natural_key_rule AS ON INSERT TO natural_key_events DO ALSO
           INSERT INTO natural_key_log
           SELECT key_value
           FROM natural_key_left AS left_source
           NATURAL JOIN natural_key_right AS right_source
           WHERE left_source.key_value = NEW.id;
         ALTER TABLE natural_key_left RENAME COLUMN key_value TO left_key;
         INSERT INTO natural_key_events VALUES (2)",
    );
    assert_eq!(
        exec(&engine, "SELECT value FROM natural_key_log").value_at(0, 0),
        Some(&Value::Int(2))
    );
    let error = engine
        .sql("ALTER TABLE natural_key_left DROP COLUMN left_key", &[])
        .expect_err("a creation-bound NATURAL JOIN key must remain an exact dependency");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
}

#[test]
fn positional_rule_insert_targets_are_bound_to_creation_columns() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rule-positional-target-columns.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE positional_events(id INTEGER);
             CREATE TABLE positional_log(first_value INTEGER, second_value INTEGER);
             CREATE RULE positional_target AS ON INSERT TO positional_events DO ALSO
               INSERT INTO positional_log VALUES (NEW.id, 7);
             ALTER TABLE positional_log RENAME COLUMN first_value TO renamed_first_value;
             ALTER TABLE positional_log RENAME COLUMN second_value TO renamed_second_value;
             ALTER TABLE positional_log ADD COLUMN later_value INTEGER DEFAULT 99;
             INSERT INTO positional_events VALUES (5)",
        );
        let row = &exec(
            &engine,
            "SELECT renamed_first_value, renamed_second_value, later_value FROM positional_log",
        )
        .rows[0];
        assert_eq!(row.get("renamed_first_value"), Some(&Value::Int(5)));
        assert_eq!(row.get("renamed_second_value"), Some(&Value::Int(7)));
        assert_eq!(row.get("later_value"), Some(&Value::Int(99)));
    }
    let engine = Engine::open(&path).expect("positional target bindings must reopen");
    let error = engine
        .sql(
            "ALTER TABLE positional_log DROP COLUMN renamed_second_value",
            &[],
        )
        .expect_err("the positional target column must remain an exact dependency");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
}

#[test]
fn rule_source_whole_rows_do_not_create_per_column_dependencies() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE whole_row_events(id INTEGER);
         CREATE TABLE whole_row_source(first_value INTEGER, disposable_value INTEGER);
         CREATE TABLE whole_row_log(payload JSONB);
         CREATE RULE whole_row_source_rule AS ON INSERT TO whole_row_events DO ALSO
           INSERT INTO whole_row_log SELECT to_jsonb(source.*)
           FROM whole_row_source AS source;
         ALTER TABLE whole_row_source DROP COLUMN disposable_value;
         INSERT INTO whole_row_source VALUES (8);
         INSERT INTO whole_row_events VALUES (1)",
    );
    assert_eq!(
        exec(&engine, "SELECT payload FROM whole_row_log").value_at(0, 0),
        Some(&Value::JsonB("{\"first_value\":8}".into()))
    );
}

#[test]
fn rule_source_projection_stars_bind_creation_columns() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE projection_star_events(id INTEGER);
         CREATE TABLE projection_star_source(first_value INTEGER, second_value INTEGER);
         CREATE TABLE projection_star_log(first_value INTEGER, second_value INTEGER);
         INSERT INTO projection_star_source VALUES (3, 4);
         CREATE RULE projection_star_rule AS ON INSERT TO projection_star_events DO ALSO
           INSERT INTO projection_star_log
           SELECT source.* FROM projection_star_source AS source;
         ALTER TABLE projection_star_source ADD COLUMN later_value INTEGER DEFAULT 99;
         INSERT INTO projection_star_events VALUES (1);
         ALTER TABLE projection_star_source DROP COLUMN later_value",
    );
    let row = &exec(
        &engine,
        "SELECT first_value, second_value FROM projection_star_log",
    )
    .rows[0];
    assert_eq!(row.get("first_value"), Some(&Value::Int(3)));
    assert_eq!(row.get("second_value"), Some(&Value::Int(4)));
    let error = engine
        .sql(
            "ALTER TABLE projection_star_source DROP COLUMN second_value",
            &[],
        )
        .expect_err("a creation-bound projection star must depend on its attributes");
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
}
