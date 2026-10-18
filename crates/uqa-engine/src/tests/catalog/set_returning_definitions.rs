//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Definitions stored while `regexp_matches` returned one value: the catalog reopens and reads keep working, while each stored call evaluated as one value reports `PostgreSQL`'s executor diagnostic.

use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use std::sync::Arc;
use uqa_execution::schema::events::persistence::{RULES_METADATA_KEY, TRIGGERS_METADATA_KEY};
use uqa_storage::CatalogFacade;

/// Rename each stored call of the scalar `regexp_match` to an unbound `regexp_matches` call, the form a catalog written before `regexp_matches` returned a set holds, and count the renamed calls.
fn rename_calls(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(object) => {
            let mut renamed = 0;
            if let Some(serde_json::Value::Object(call)) = object.get_mut("Func") {
                if call.get("name").and_then(serde_json::Value::as_str) == Some("regexp_match") {
                    call.insert("name".into(), "regexp_matches".into());
                    call.remove("binding");
                    renamed += 1;
                }
            }
            renamed + object.values_mut().map(rename_calls).sum::<usize>()
        }
        serde_json::Value::Array(items) => items.iter_mut().map(rename_calls).sum(),
        _ => 0,
    }
}

pub(super) fn legacy_calls(json: &str) -> (String, usize) {
    let mut value: serde_json::Value = serde_json::from_str(json).unwrap();
    let renamed = rename_calls(&mut value);
    (serde_json::to_string(&value).unwrap(), renamed)
}

/// The legacy form of a stored rule catalog: its calls as [`legacy_calls`] renames them, in rule catalog format 3, which earlier releases wrote.
pub(super) fn legacy_rule_catalog(json: &str) -> (String, usize) {
    let (rules, renamed) = legacy_calls(json);
    let mut value: serde_json::Value = serde_json::from_str(&rules).unwrap();
    value["format_version"] = serde_json::Value::from(3);
    (serde_json::to_string(&value).unwrap(), renamed)
}

fn rows(engine: &Engine, statement: &str) -> Vec<uqa_sql::ResultRow> {
    sql(engine, statement).rows
}

/// Definitions that call the scalar `regexp_match`, which the rewrite below turns into the stored form of a scalar `regexp_matches` call, and rows they produced.
const DEFINITIONS: &str = "CREATE TABLE d (k int, a text[] DEFAULT regexp_match('zz', 'z')); \
     CREATE TABLE g (k int, s text, a text[] GENERATED ALWAYS AS (regexp_match(s, 'z')) STORED); \
     CREATE TABLE ix (k int, s text); \
     CREATE INDEX ix_expr ON ix ((regexp_match(s, 'z'))); \
     CREATE INDEX ix_pred ON ix (k) WHERE regexp_match(s, 'z') IS NOT NULL; \
     CREATE TABLE tw (k int, s text); \
     CREATE FUNCTION bump() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.k := NEW.k + 100; RETURN NEW; END $$; \
     CREATE TRIGGER tw_when BEFORE INSERT ON tw FOR EACH ROW WHEN (regexp_match(NEW.s, 'z') IS NOT NULL) EXECUTE FUNCTION bump(); \
     CREATE TABLE rl (k int, s text); \
     CREATE TABLE rl_log (k int); \
     CREATE RULE rl_rule AS ON INSERT TO rl WHERE regexp_match(NEW.s, 'z') IS NOT NULL DO ALSO INSERT INTO rl_log VALUES (NEW.k); \
     CREATE TABLE pk (s text) PARTITION BY LIST ((regexp_match(s, 'z'))); \
     CREATE FUNCTION fdef(x text[] DEFAULT regexp_match('zz', 'z')) RETURNS text[] LANGUAGE sql AS 'SELECT x'; \
     INSERT INTO d (k) VALUES (1); \
     INSERT INTO g (k, s) VALUES (1, 'zz'); \
     INSERT INTO ix VALUES (1, 'zz'); \
     INSERT INTO tw VALUES (1, 'zz'); \
     INSERT INTO rl VALUES (1, 'zz')";

/// Rewrite every stored definition of [`DEFINITIONS`] into its legacy form.
fn store_legacy_definitions(catalog: &dyn CatalogFacade) {
    for mut table in catalog.load_tables().unwrap() {
        let name = table.relation.qualified_name();
        let (columns, in_columns) = legacy_calls(&table.columns_json);
        let (constraints, in_constraints) = legacy_calls(&table.constraints_json);
        let expected = match name.as_str() {
            "public.d" | "public.g" => (1, 0),
            "public.pk" => (0, 1),
            _ => (0, 0),
        };
        assert_eq!((in_columns, in_constraints), expected, "{name}");
        if expected != (0, 0) {
            table.columns_json = columns;
            table.constraints_json = constraints;
            catalog.save_table(&table).unwrap();
        }
    }
    let mut indexes = 0;
    for mut index in catalog.load_catalog_indexes().unwrap() {
        let (columns, in_columns) = legacy_calls(&index.columns_json);
        let (definition, in_definition) = legacy_calls(index.definition_json.as_deref().unwrap());
        if in_columns + in_definition > 0 {
            indexes += 1;
            index.columns_json = columns;
            index.definition_json = Some(definition);
            catalog.save_catalog_index_row(&index).unwrap();
        }
    }
    assert_eq!(indexes, 2);
    for key in [
        TRIGGERS_METADATA_KEY,
        RULES_METADATA_KEY,
        "sql_functions_json",
    ] {
        let stored = catalog.get_metadata(key).unwrap().unwrap();
        let (legacy, renamed) = if key == RULES_METADATA_KEY {
            legacy_rule_catalog(&stored)
        } else {
            legacy_calls(&stored)
        };
        assert!(renamed > 0, "{key}");
        catalog.set_metadata(key, &legacy).unwrap();
    }
}

/// Reading the stored rows evaluates no stored call.
fn assert_reads(engine: &Engine) {
    for (stored, expected) in [
        ("SELECT k, a FROM d", "SELECT 1 AS k, ARRAY['z'] AS a"),
        (
            "SELECT k, s, a FROM g",
            "SELECT 1 AS k, 'zz' AS s, ARRAY['z'] AS a",
        ),
        ("SELECT k, s FROM tw", "SELECT 101 AS k, 'zz' AS s"),
        ("SELECT k FROM rl_log", "SELECT 1 AS k"),
        ("SELECT k FROM ix WHERE k = 1", "SELECT 1 AS k"),
        ("SELECT fdef(ARRAY['q']) AS v", "SELECT ARRAY['q'] AS v"),
    ] {
        assert_eq!(rows(engine, stored), rows(engine, expected), "{stored}");
    }
}

#[test]
fn legacy_set_returning_definitions_reopen_and_fail_only_when_evaluated() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, DEFINITIONS);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        store_legacy_definitions(raw.catalog.as_ref());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_reads(&reopened);
        // Each statement that evaluates a stored call reports the executor diagnostic.
        for statement in [
            "INSERT INTO d (k) VALUES (2)",
            "INSERT INTO g (k, s) VALUES (2, 'zz')",
            "UPDATE g SET s = 'zzz'",
            "INSERT INTO ix VALUES (2, 'zz')",
            "INSERT INTO tw VALUES (2, 'zz')",
            "INSERT INTO rl VALUES (2, 'zz')",
            "INSERT INTO pk VALUES ('zz')",
            "SELECT fdef()",
            "SELECT * FROM fdef()",
        ] {
            let error = reopened.sql(statement, &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some("0A000"), "{statement}: {error}");
            assert_eq!(
                error.to_string(),
                "set-valued function called in context that cannot accept a set",
                "{statement}"
            );
        }
        // Statements that evaluate no stored call keep working.
        sql(
            &reopened,
            "INSERT INTO d VALUES (3, ARRAY['x']); UPDATE d SET k = 4 WHERE k = 3; UPDATE g SET k = 5; DELETE FROM g WHERE k = 5; DELETE FROM tw",
        );
        assert_eq!(
            rows(&reopened, "SELECT k, a FROM d ORDER BY k"),
            rows(
                &reopened,
                "SELECT * FROM (VALUES (1, ARRAY['z']), (4, ARRAY['x'])) AS v(k, a)"
            )
        );
        assert!(sql(&reopened, "SELECT k FROM g").rows.is_empty());
        assert!(sql(&reopened, "SELECT k FROM tw").rows.is_empty());
    }
}
