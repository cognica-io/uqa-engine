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
