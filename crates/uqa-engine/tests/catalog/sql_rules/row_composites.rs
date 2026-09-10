//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn scalar_rule_rows_follow_the_live_event_composite_lifecycle() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("rule-scalar-row-lifecycle.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(
            &engine,
            "CREATE TABLE scalar_event(a INTEGER PRIMARY KEY, b TEXT);
             CREATE TABLE scalar_log(stage INTEGER, payload JSONB);
             CREATE RULE scalar_row AS ON INSERT TO scalar_event DO ALSO
               INSERT INTO scalar_log VALUES (NEW.a, to_jsonb(NEW.*));
             ALTER TABLE scalar_event ADD COLUMN c INTEGER DEFAULT 7;
             INSERT INTO scalar_event(a, b) VALUES (1, 'one')",
        );
        assert_eq!(
            exec(
                &engine,
                "SELECT payload = '{\"a\":1,\"b\":\"one\",\"c\":7}'::jsonb AS matches
                 FROM scalar_log WHERE stage = 1",
            )
            .value_at(0, 0),
            Some(&Value::Bool(true))
        );
        exec(
            &engine,
            "ALTER TABLE scalar_event RENAME COLUMN b TO renamed;
             INSERT INTO scalar_event(a, renamed, c) VALUES (2, 'two', 8)",
        );
        assert_eq!(
            exec(
                &engine,
                "SELECT payload = '{\"a\":2,\"renamed\":\"two\",\"c\":8}'::jsonb AS matches
                 FROM scalar_log WHERE stage = 2",
            )
            .value_at(0, 0),
            Some(&Value::Bool(true))
        );
        exec(
            &engine,
            "ALTER TABLE scalar_event DROP COLUMN renamed RESTRICT;
             INSERT INTO scalar_event(a, c) VALUES (3, 9)",
        );
        assert_eq!(
            exec(
                &engine,
                "SELECT payload = '{\"a\":3,\"c\":9}'::jsonb AS matches
                 FROM scalar_log WHERE stage = 3",
            )
            .value_at(0, 0),
            Some(&Value::Bool(true))
        );
    }

    let engine = Engine::open(&database).expect("a scalar rule-row reference must restore");
    exec(&engine, "INSERT INTO scalar_event(a, c) VALUES (4, 10)");
    assert_eq!(
        exec(
            &engine,
            "SELECT payload = '{\"a\":4,\"c\":10}'::jsonb AS matches
             FROM scalar_log WHERE stage = 4",
        )
        .value_at(0, 0),
        Some(&Value::Bool(true))
    );
}

#[test]
fn scalar_rule_rows_drive_conditions_and_set_oriented_actions() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE composite_condition_event(a INTEGER PRIMARY KEY, b TEXT);
         CREATE TABLE composite_condition_log(payload JSONB);
         INSERT INTO composite_condition_event VALUES (1, 'one'), (2, 'two');
         CREATE RULE composite_condition AS ON UPDATE TO composite_condition_event
           WHERE (OLD IS DISTINCT FROM NEW) DO ALSO
           INSERT INTO composite_condition_log VALUES (to_jsonb(NEW))",
    );
    exec(
        &engine,
        "UPDATE composite_condition_event SET b = b WHERE a = 1",
    );
    assert_eq!(
        exec(&engine, "SELECT count(*) AS n FROM composite_condition_log").value_at(0, 0),
        Some(&Value::Int(0))
    );
    exec(&engine, "UPDATE composite_condition_event SET b = b || '!'");
    assert_eq!(
        exec(&engine, "SELECT count(*) AS n FROM composite_condition_log").value_at(0, 0),
        Some(&Value::Int(2))
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT bool_and(payload = to_jsonb(composite_condition_event.*)) AS matches
             FROM composite_condition_log
             JOIN composite_condition_event ON (payload->>'a')::INTEGER = a",
        )
        .value_at(0, 0),
        Some(&Value::Bool(true))
    );
    exec(
        &engine,
        "TRUNCATE composite_condition_log;
         ALTER TABLE composite_condition_event ADD COLUMN c INTEGER DEFAULT 7;
         UPDATE composite_condition_event SET c = 8 WHERE a = 1",
    );
    assert_eq!(
        exec(
            &engine,
            "SELECT payload = '{\"a\":1,\"b\":\"one!\",\"c\":8}'::jsonb AS matches
             FROM composite_condition_log",
        )
        .value_at(0, 0),
        Some(&Value::Bool(true))
    );
}

#[test]
fn scalar_rule_row_sides_and_local_shadowing_match_postgresql() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE composite_scope_event(a INTEGER, b TEXT);
         CREATE TABLE composite_scope_log(payload JSONB);
         CREATE TABLE composite_scope_local(new TEXT);
         CREATE TABLE composite_scope_old(old TEXT);
         INSERT INTO composite_scope_local VALUES ('table-local');
         INSERT INTO composite_scope_old VALUES ('old-local')",
    );
    for (label, sql) in [
        (
            "INSERT OLD",
            "CREATE RULE invalid_old_bare AS ON INSERT TO composite_scope_event DO ALSO
             INSERT INTO composite_scope_log VALUES (to_jsonb(OLD))",
        ),
        (
            "INSERT OLD.*",
            "CREATE RULE invalid_old_star AS ON INSERT TO composite_scope_event DO ALSO
             INSERT INTO composite_scope_log VALUES (to_jsonb(OLD.*))",
        ),
        (
            "DELETE NEW",
            "CREATE RULE invalid_new_bare AS ON DELETE TO composite_scope_event DO ALSO
             INSERT INTO composite_scope_log VALUES (to_jsonb(NEW))",
        ),
        (
            "DELETE NEW.*",
            "CREATE RULE invalid_new_star AS ON DELETE TO composite_scope_event DO ALSO
             INSERT INTO composite_scope_log VALUES (to_jsonb(NEW.*))",
        ),
    ] {
        let error = engine.sql(sql, &[]).expect_err(label);
        assert_eq!(error.sqlstate(), Some("42P17"), "{label}: {error}");
    }

    exec(
        &engine,
        "CREATE RULE nested_bare_row AS ON INSERT TO composite_scope_event DO ALSO
           INSERT INTO composite_scope_log VALUES
             ((SELECT to_jsonb(new) FROM (VALUES (42)) AS new(x)));
         CREATE RULE nested_star_row AS ON INSERT TO composite_scope_event DO ALSO
           INSERT INTO composite_scope_log VALUES
             ((SELECT to_jsonb(new.*) FROM (VALUES (43)) AS new(x)));
         CREATE RULE nested_column AS ON INSERT TO composite_scope_event DO ALSO
           INSERT INTO composite_scope_log VALUES
             ((SELECT to_jsonb(new) FROM (VALUES ('local')) AS value(new)));
         CREATE RULE nested_table_column AS ON INSERT TO composite_scope_event DO ALSO
           INSERT INTO composite_scope_log
             SELECT to_jsonb(new) FROM composite_scope_local;
         CREATE RULE nested_invalid_side_column AS ON INSERT TO composite_scope_event DO ALSO
           INSERT INTO composite_scope_log
             SELECT to_jsonb(old) FROM composite_scope_old;
         CREATE RULE nested_derived_column AS ON INSERT TO composite_scope_event DO ALSO
           INSERT INTO composite_scope_log VALUES
             ((SELECT to_jsonb(new) FROM (SELECT 'derived'::TEXT AS new) AS value));
         CREATE RULE nested_cte_column AS ON INSERT TO composite_scope_event DO ALSO
           WITH value(new) AS (VALUES ('cte'))
           INSERT INTO composite_scope_log SELECT to_jsonb(new) FROM value;
         INSERT INTO composite_scope_event VALUES (1, 'event')",
    );
    assert_eq!(
        strings(
            &engine,
            "SELECT payload::TEXT AS value FROM composite_scope_log ORDER BY value",
            "value",
        ),
        [
            "\"cte\"",
            "\"derived\"",
            "\"local\"",
            "\"old-local\"",
            "\"table-local\"",
            "{\"x\": 42}",
            "{\"x\": 43}"
        ]
    );
}

#[test]
fn scalar_action_returning_uses_event_and_action_whole_rows() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE scalar_return_insert_event(payload JSONB);
         CREATE TABLE scalar_return_insert_target(i INTEGER);
         CREATE RULE scalar_return_insert AS ON INSERT TO scalar_return_insert_event DO INSTEAD
           INSERT INTO scalar_return_insert_target VALUES (42) RETURNING to_jsonb(NEW)",
    );
    assert_eq!(
        exec(
            &engine,
            "INSERT INTO scalar_return_insert_event VALUES ('{}') RETURNING payload",
        )
        .value_at(0, 0),
        Some(&Value::JsonB("{\"i\":42}".into()))
    );

    exec(
        &engine,
        "CREATE TABLE scalar_return_update_event(payload JSONB);
         CREATE TABLE scalar_return_update_target(i INTEGER);
         INSERT INTO scalar_return_update_event VALUES ('{\"before\":1}');
         INSERT INTO scalar_return_update_target VALUES (7);
         CREATE RULE scalar_return_update AS ON UPDATE TO scalar_return_update_event DO INSTEAD
           UPDATE scalar_return_update_target SET i = i + 1 RETURNING to_jsonb(NEW)",
    );
    assert_eq!(
        exec(
            &engine,
            "UPDATE scalar_return_update_event SET payload = '{\"after\":2}' RETURNING payload",
        )
        .value_at(0, 0),
        Some(&Value::JsonB("{\"payload\":{\"after\":2}}".into()))
    );

    exec(
        &engine,
        "CREATE TABLE scalar_return_alias_event(payload JSONB);
         CREATE TABLE scalar_return_alias_target(i INTEGER);
         INSERT INTO scalar_return_alias_event VALUES ('{\"before\":1}');
         INSERT INTO scalar_return_alias_target VALUES (8);
         CREATE RULE scalar_return_alias AS ON UPDATE TO scalar_return_alias_event DO INSTEAD
           UPDATE scalar_return_alias_target SET i = i + 1
           RETURNING WITH (NEW AS action_new) to_jsonb(action_new)",
    );
    assert_eq!(
        exec(
            &engine,
            "UPDATE scalar_return_alias_event SET payload = '{\"after\":2}' RETURNING payload",
        )
        .value_at(0, 0),
        Some(&Value::JsonB("{\"i\":9}".into()))
    );
}

#[test]
fn dml_whole_row_images_exclude_system_attributes_but_expose_them_individually() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE system_image(i INTEGER)");
    let inserted = exec(
        &engine,
        "INSERT INTO system_image VALUES (6)
         RETURNING to_jsonb(NEW) AS image,
                   NEW.xmin IS NOT NULL AS has_xmin,
                   NEW.tableoid = 'system_image'::regclass AS has_tableoid",
    );
    assert_eq!(
        inserted.value_at(0, 0),
        Some(&Value::JsonB("{\"i\":6}".into()))
    );
    assert_eq!(inserted.value_at(0, 1), Some(&Value::Bool(true)));
    assert_eq!(inserted.value_at(0, 2), Some(&Value::Bool(true)));
}

#[test]
fn action_target_returning_stars_are_creation_bound_and_column_dependent() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("rule-action-returning-star.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(
            &engine,
            "CREATE TABLE target_star_event(a INTEGER PRIMARY KEY, b TEXT);
             CREATE TABLE target_star_action(i INTEGER PRIMARY KEY, y TEXT);
             CREATE RULE target_star_provider AS ON INSERT TO target_star_event DO INSTEAD
               INSERT INTO target_star_action VALUES (NEW.a, NEW.b) RETURNING *;
             ALTER TABLE target_star_action ADD COLUMN later INTEGER DEFAULT 9",
        );
        let inserted = exec(
            &engine,
            "INSERT INTO target_star_event VALUES (1, 'one') RETURNING a, b",
        );
        assert_eq!(inserted.value_at(0, 0), Some(&Value::Int(1)));
        assert_eq!(inserted.value_at(0, 1), Some(&Value::Str("one".into())));
        assert_eq!(
            exec(
                &engine,
                "SELECT i || ':' || y || ':' || later AS value FROM target_star_action",
            )
            .value_at(0, 0),
            Some(&Value::Str("1:one:9".into()))
        );
    }

    let engine = Engine::open(&database).expect("expanded target RETURNING stars must restore");
    exec(
        &engine,
        "ALTER TABLE target_star_action RENAME COLUMN y TO renamed",
    );
    let inserted = exec(
        &engine,
        "INSERT INTO target_star_event VALUES (2, 'two') RETURNING a, b",
    );
    assert_eq!(inserted.value_at(0, 0), Some(&Value::Int(2)));
    assert_eq!(inserted.value_at(0, 1), Some(&Value::Str("two".into())));
    let dependent = engine
        .sql(
            "ALTER TABLE target_star_action DROP COLUMN renamed RESTRICT",
            &[],
        )
        .expect_err("the expanded target star must depend on its creation-time columns");
    assert_eq!(dependent.sqlstate(), Some("2BP01"), "{dependent}");
    exec(
        &engine,
        "ALTER TABLE target_star_action DROP COLUMN renamed CASCADE",
    );
    assert!(exec(
        &engine,
        "SELECT oid FROM pg_rewrite WHERE rulename = 'target_star_provider'",
    )
    .rows
    .is_empty());
}

#[test]
fn action_target_returning_star_namespaces_expand_equivalently() {
    let engine = Engine::new();
    for (suffix, returning) in [
        ("new_image", "NEW.*"),
        ("target_alias", "target.*"),
        ("explicit_image", "WITH (NEW AS action_new) action_new.*"),
    ] {
        exec(
            &engine,
            &format!(
                "CREATE TABLE star_{suffix}_event(a INTEGER, b TEXT);
                 CREATE TABLE star_{suffix}_action(i INTEGER, y TEXT);
                 CREATE RULE star_{suffix}_provider AS ON INSERT TO star_{suffix}_event DO INSTEAD
                   INSERT INTO star_{suffix}_action AS target VALUES (NEW.a, NEW.b)
                   RETURNING {returning};
                 ALTER TABLE star_{suffix}_action ADD COLUMN later INTEGER DEFAULT 9",
            ),
        );
        let inserted = exec(
            &engine,
            &format!("INSERT INTO star_{suffix}_event VALUES (1, '{suffix}') RETURNING a, b"),
        );
        assert_eq!(inserted.value_at(0, 0), Some(&Value::Int(1)));
        assert_eq!(inserted.value_at(0, 1), Some(&Value::Str(suffix.into())));
    }
}
