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
        let definition = exec(
            &engine,
            "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite WHERE rulename = 'rename_dependency_rule'",
        );
        let Some(Value::Str(definition)) = definition.rows[0].get("definition") else {
            panic!("expected rule definition text");
        };
        assert!(
            definition.contains("renamed_dependency_source"),
            "{definition}"
        );
        assert!(
            definition.contains("renamed_dependency_log"),
            "{definition}"
        );
        assert!(
            !definition.contains("FROM rename_dependency_source"),
            "{definition}"
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

fn create_relation_kind_rule_fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE relation_kind_base(id INTEGER);
         INSERT INTO relation_kind_base VALUES (11);
         CREATE VIEW relation_kind_view AS SELECT id FROM relation_kind_base;
         CREATE MATERIALIZED VIEW relation_kind_materialized AS SELECT id FROM relation_kind_base;
         CREATE SERVER relation_kind_memory FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory');
         CREATE FOREIGN TABLE relation_kind_foreign(id INTEGER) SERVER relation_kind_memory OPTIONS (source 'memory');
         CREATE VIEW relation_kind_view_wrapper AS SELECT id FROM relation_kind_view;
         CREATE VIEW relation_kind_materialized_wrapper AS SELECT id FROM relation_kind_materialized;
         CREATE VIEW relation_kind_foreign_wrapper AS SELECT id FROM relation_kind_foreign;
         CREATE TABLE relation_kind_view_event_log(id INTEGER);
         CREATE VIEW relation_kind_event_view AS SELECT id FROM relation_kind_view_event_log;
         CREATE RULE relation_kind_event_rule AS ON INSERT TO relation_kind_event_view DO INSTEAD
           INSERT INTO relation_kind_view_event_log VALUES (NEW.id);
         CREATE TABLE relation_kind_events(id INTEGER);
         CREATE TABLE relation_kind_log(value INTEGER);
         CREATE RULE relation_kind_view_rule AS ON INSERT TO relation_kind_events DO ALSO
           INSERT INTO relation_kind_log SELECT id FROM relation_kind_view;
         CREATE RULE relation_kind_materialized_rule AS ON INSERT TO relation_kind_events DO ALSO
           INSERT INTO relation_kind_log SELECT id FROM relation_kind_materialized;
         CREATE RULE relation_kind_foreign_rule AS ON INSERT TO relation_kind_events DO ALSO
           INSERT INTO relation_kind_log SELECT id FROM relation_kind_foreign",
    );
    engine
        .load_memory_foreign_table(
            "relation_kind_foreign",
            vec![std::collections::BTreeMap::from([(
                "id".into(),
                Value::Int(33),
            )])],
        )
        .unwrap();
    let error = engine
        .sql(
            "CREATE RULE invalid_foreign_event AS ON INSERT TO relation_kind_foreign DO NOTHING",
            &[],
        )
        .expect_err("a foreign table cannot be a rule event relation");
    assert_eq!(error.sqlstate(), Some("42809"), "{error}");
}

fn assert_renamed_relation_kind_rule_sources(engine: &Engine, identities: &[(Value, Value); 3]) {
    for (index, name) in [
        "renamed_relation_kind_view",
        "renamed_relation_kind_materialized",
        "renamed_relation_kind_foreign",
    ]
    .into_iter()
    .enumerate()
    {
        let row = exec(
            engine,
            &format!("SELECT oid, reltype FROM pg_class WHERE oid = '{name}'::regclass"),
        )
        .rows
        .remove(0);
        assert_eq!(row.get("oid"), Some(&identities[index].0), "{name}");
        assert_eq!(row.get("reltype"), Some(&identities[index].1), "{name}");
    }
    assert_eq!(
        exec(
            engine,
            "SELECT id FROM relation_kind_view_wrapper
             UNION ALL SELECT id FROM relation_kind_materialized_wrapper
             UNION ALL SELECT id FROM relation_kind_foreign_wrapper
             ORDER BY id",
        )
        .rows
        .iter()
        .map(|row| row.get("id"))
        .collect::<Vec<_>>(),
        [
            Some(&Value::Int(11)),
            Some(&Value::Int(11)),
            Some(&Value::Int(33))
        ]
    );
    exec(engine, "INSERT INTO relation_kind_events VALUES (1)");
    assert_eq!(
        exec(engine, "SELECT value FROM relation_kind_log ORDER BY value")
            .rows
            .iter()
            .map(|row| row.get("value"))
            .collect::<Vec<_>>(),
        [
            Some(&Value::Int(11)),
            Some(&Value::Int(11)),
            Some(&Value::Int(33))
        ]
    );
    let definitions = strings(
        engine,
        "SELECT pg_get_ruledef(oid, true) AS definition FROM pg_rewrite
         WHERE rulename LIKE 'relation_kind_%_rule' ORDER BY rulename",
        "definition",
    );
    assert!(definitions.iter().all(|definition| {
        !definition.contains("FROM relation_kind_view")
            && !definition.contains("FROM relation_kind_materialized")
            && !definition.contains("FROM relation_kind_foreign")
    }));
    assert!(
        definitions.iter().any(|definition| {
            definition.contains("relation_kind_event_rule")
                && definition.contains("ON INSERT TO renamed_relation_kind_event_view")
        }),
        "{definitions:?}"
    );
}

fn assert_reopened_relation_kind_rule_dependencies(
    engine: &Engine,
    identities: &[(Value, Value); 3],
) {
    engine
        .load_memory_foreign_table(
            "renamed_relation_kind_foreign",
            vec![std::collections::BTreeMap::from([(
                "id".into(),
                Value::Int(33),
            )])],
        )
        .unwrap();
    for (index, name) in [
        "renamed_relation_kind_view",
        "renamed_relation_kind_materialized",
        "renamed_relation_kind_foreign",
    ]
    .into_iter()
    .enumerate()
    {
        let row = exec(
            engine,
            &format!("SELECT oid, reltype FROM pg_class WHERE oid = '{name}'::regclass"),
        )
        .rows
        .remove(0);
        assert_eq!(row.get("oid"), Some(&identities[index].0), "{name}");
        assert_eq!(row.get("reltype"), Some(&identities[index].1), "{name}");
        let drop = match index {
            0 => format!("DROP VIEW {name}"),
            1 => format!("DROP MATERIALIZED VIEW {name}"),
            _ => format!("DROP FOREIGN TABLE {name}"),
        };
        let error = engine
            .sql(&drop, &[])
            .expect_err("the renamed relation must remain protected by rule dependencies");
        assert_eq!(error.sqlstate(), Some("2BP01"), "{drop}: {error}");
    }
    exec(engine, "INSERT INTO relation_kind_events VALUES (2)");
    exec(
        engine,
        "INSERT INTO renamed_relation_kind_event_view VALUES (55)",
    );
    assert_eq!(
        exec(
            engine,
            "SELECT id FROM relation_kind_view_event_log ORDER BY id"
        )
        .rows
        .iter()
        .map(|row| row.get("id"))
        .collect::<Vec<_>>(),
        [Some(&Value::Int(44)), Some(&Value::Int(55))]
    );
    assert_eq!(
        exec(
            engine,
            "SELECT value, count(*) AS count FROM relation_kind_log GROUP BY value ORDER BY value",
        )
        .rows
        .iter()
        .map(|row| (row.get("value"), row.get("count")))
        .collect::<Vec<_>>(),
        [
            (Some(&Value::Int(11)), Some(&Value::Int(4))),
            (Some(&Value::Int(33)), Some(&Value::Int(2)))
        ]
    );
    exec(
        engine,
        "DROP VIEW renamed_relation_kind_view CASCADE;
         DROP MATERIALIZED VIEW renamed_relation_kind_materialized CASCADE;
         DROP FOREIGN TABLE renamed_relation_kind_foreign CASCADE;
         DROP VIEW renamed_relation_kind_event_view",
    );
    assert!(exec(
        engine,
        "SELECT rulename FROM pg_rewrite WHERE rulename LIKE 'relation_kind_%_rule'",
    )
    .rows
    .is_empty());
}

#[test]
fn rule_dependencies_follow_view_materialized_view_and_foreign_table_renames() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("rule-relation-kind-rename.uqa");
    let identities;
    {
        let engine = Engine::open(&path).unwrap();
        create_relation_kind_rule_fixture(&engine);

        identities = [
            "relation_kind_view",
            "relation_kind_materialized",
            "relation_kind_foreign",
        ]
        .map(|name| {
            let row = exec(
                &engine,
                &format!("SELECT oid, reltype FROM pg_class WHERE oid = '{name}'::regclass"),
            )
            .rows
            .remove(0);
            (
                row.get("oid").cloned().unwrap(),
                row.get("reltype").cloned().unwrap(),
            )
        });

        exec(
            &engine,
            "ALTER VIEW relation_kind_view RENAME TO renamed_relation_kind_view;
             ALTER MATERIALIZED VIEW relation_kind_materialized RENAME TO renamed_relation_kind_materialized;
             ALTER FOREIGN TABLE relation_kind_foreign RENAME TO renamed_relation_kind_foreign;
             ALTER VIEW relation_kind_event_view RENAME TO renamed_relation_kind_event_view;
             INSERT INTO renamed_relation_kind_event_view VALUES (44);
             CREATE VIEW relation_kind_view AS SELECT 101 AS id;
             CREATE MATERIALIZED VIEW relation_kind_materialized AS SELECT 102 AS id;
             CREATE FOREIGN TABLE relation_kind_foreign(id INTEGER) SERVER relation_kind_memory OPTIONS (source 'memory')",
        );
        assert_eq!(
            exec(&engine, "SELECT id FROM relation_kind_view_event_log").rows[0].get("id"),
            Some(&Value::Int(44))
        );
        assert_renamed_relation_kind_rule_sources(&engine, &identities);

        exec(
            &engine,
            "BEGIN;
             ALTER VIEW renamed_relation_kind_view RENAME TO rolled_back_relation_kind_view;
             ROLLBACK",
        );
        assert!(exec(
            &engine,
            "SELECT to_regclass('renamed_relation_kind_view') IS NOT NULL AS present",
        )
        .rows[0]
            .get("present")
            .is_some_and(|value| value == &Value::Bool(true)));
    }

    let engine = Engine::open(&path).expect("renamed relation-kind rule dependencies must restore");
    assert_reopened_relation_kind_rule_dependencies(&engine, &identities);
}

fn rename_relation_kind_with_rollback(
    engine: &Engine,
    kind: &str,
    name: &str,
    value: i64,
) -> (String, uqa_engine::SQLResult) {
    let renamed = format!("renamed_{name}");
    let original = exec(
        engine,
        &format!("SELECT oid, reltype FROM pg_class WHERE oid = '{name}'::regclass"),
    );
    for target in [name, "rename_base"] {
        let error = engine
            .sql(&format!("ALTER {kind} {name} RENAME TO {target}"), &[])
            .expect_err("rename must reject an occupied relation name");
        assert_eq!(error.sqlstate(), Some("42P07"), "{error}");
    }
    exec(
        engine,
        &format!(
            "BEGIN;
             ALTER TABLE {name} RENAME TO {renamed};
             SAVEPOINT retained_name;
             ALTER {kind} {renamed} RENAME TO intermediate_name;
             ROLLBACK TO retained_name"
        ),
    );
    assert_eq!(
        exec(engine, &format!("SELECT id FROM {renamed}")).rows[0]["id"],
        Value::Int(value)
    );
    exec(engine, "ROLLBACK");
    assert_eq!(
        exec(engine, &format!("SELECT id FROM {name}")).rows[0]["id"],
        Value::Int(value)
    );
    exec(engine, &format!("ALTER TABLE {name} RENAME TO {renamed}"));
    assert_eq!(
        exec(
            engine,
            &format!("SELECT oid, reltype FROM pg_class WHERE oid = '{renamed}'::regclass"),
        )
        .rows,
        original.rows
    );
    (renamed, original)
}

fn assert_missing_relation_rename_semantics(engine: &Engine) {
    for kind in ["VIEW", "MATERIALIZED VIEW", "FOREIGN TABLE", "TABLE"] {
        engine.take_sql_notices();
        exec(
            engine,
            &format!("ALTER {kind} IF EXISTS missing_rename_schema.missing_relation RENAME TO missing_target"),
        );
        assert_eq!(
            engine.take_sql_notices(),
            [(
                "NOTICE".into(),
                "relation \"missing_relation\" does not exist, skipping".into()
            )],
            "{kind}"
        );
        let error = engine
            .sql(
                &format!(
                    "ALTER {kind} missing_rename_schema.missing_relation RENAME TO missing_target"
                ),
                &[],
            )
            .expect_err("a missing schema without IF EXISTS must fail");
        assert_eq!(error.sqlstate(), Some("3F000"), "{kind}: {error}");
    }
}

#[test]
fn relation_kind_renames_preserve_identity_across_table_syntax_and_rollback() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("relation-rename-lifecycle.uqa");
    let engine = Engine::open(&path).unwrap();
    exec(
        &engine,
        "CREATE TABLE rename_base(id INTEGER);
         INSERT INTO rename_base VALUES (7);
         CREATE VIEW rename_view AS SELECT id FROM rename_base;
         CREATE MATERIALIZED VIEW rename_materialized AS SELECT id FROM rename_base;
         CREATE SERVER rename_memory FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory');
         CREATE FOREIGN TABLE rename_foreign(id SERIAL) SERVER rename_memory OPTIONS (source 'memory')",
    );
    engine
        .load_memory_foreign_table(
            "rename_foreign",
            vec![std::collections::BTreeMap::from([(
                "id".into(),
                Value::Int(13),
            )])],
        )
        .unwrap();
    let mut identities = Vec::new();
    for (kind, name, value) in [
        ("VIEW", "rename_view", 7),
        ("MATERIALIZED VIEW", "rename_materialized", 7),
        ("FOREIGN TABLE", "rename_foreign", 13),
    ] {
        identities.push(rename_relation_kind_with_rollback(
            &engine, kind, name, value,
        ));
    }
    assert_eq!(
        strings(
            &engine,
            "SELECT pg_get_serial_sequence('renamed_rename_foreign', 'id') AS sequence",
            "sequence",
        ),
        ["public.rename_foreign_id_seq"]
    );
    exec(
        &engine,
        "CREATE OR REPLACE VIEW renamed_rename_view AS SELECT id FROM rename_base WHERE id > 0;
         CREATE TEMPORARY VIEW rename_temporary AS SELECT id FROM rename_base;
         ALTER TABLE rename_temporary RENAME TO renamed_temporary",
    );
    assert_eq!(
        exec(&engine, "SELECT id FROM renamed_temporary").rows[0]["id"],
        Value::Int(7)
    );
    assert_missing_relation_rename_semantics(&engine);
    drop(engine);

    let engine = Engine::open(&path).unwrap();
    for (name, identity) in identities {
        assert_eq!(
            exec(
                &engine,
                &format!("SELECT oid, reltype FROM pg_class WHERE oid = '{name}'::regclass"),
            )
            .rows,
            identity.rows
        );
        assert_eq!(
            exec(&engine, &format!("SELECT count(*) AS count FROM pg_attribute WHERE attrelid = '{name}'::regclass AND attnum > 0"))
                .rows[0]["count"],
            Value::Int(1)
        );
    }
    exec(&engine, "DROP FOREIGN TABLE renamed_rename_foreign");
    assert_eq!(
        exec(
            &engine,
            "SELECT to_regclass('rename_foreign_id_seq') AS sequence"
        )
        .rows[0]["sequence"],
        Value::Null
    );
    assert_eq!(
        exec(&engine, "SELECT to_regclass('renamed_temporary') AS view").rows[0]["view"],
        Value::Null
    );
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
