//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Views, SQL-standard routine bodies and rule actions stored with a set-returning call where parse analysis now rejects one: the catalog still loads and answers its queries, DROP VIEW still works, and evaluating such a call reports the executor's diagnostic wherever the executor cannot expand it.

use super::set_returning_definitions::{legacy_calls, legacy_rule_catalog};
use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use std::sync::Arc;
use uqa_execution::schema::events::persistence::RULES_METADATA_KEY;
use uqa_storage::CatalogFacade;

/// One placement of a set-returning call that parse analysis rejects, as a query of one boolean column, and whether the executor expands the call into rows (`true`) or reports that the context cannot accept a set.
const PLACEMENTS: &[(&str, &str, bool)] = &[
    (
        "and",
        "SELECT regexp_match('a', 'a') IS NOT NULL AND true AS r",
        true,
    ),
    (
        "or",
        "SELECT regexp_match('a', 'a') IS NULL OR false AS r",
        true,
    ),
    (
        "not",
        "SELECT NOT (regexp_match('a', 'a') IS NULL) AS r",
        true,
    ),
    (
        "between",
        "SELECT regexp_match('a', 'a') BETWEEN ARRAY['a'] AND ARRAY['b'] AS r",
        true,
    ),
    (
        "is_true",
        "SELECT (regexp_match('a', 'a') IS NOT NULL) IS TRUE AS r",
        true,
    ),
    (
        "row_comparison",
        "SELECT ROW(regexp_match('a', 'a'), 1) = ROW(ARRAY['a'], 1) AS r",
        true,
    ),
    (
        "values",
        "SELECT r FROM (VALUES (regexp_match('a', 'a') IS NOT NULL), (false)) v(r)",
        false,
    ),
    (
        "filter",
        "SELECT count(*) FILTER (WHERE regexp_match('a', 'a') IS NOT NULL) > 0 AS r",
        false,
    ),
    (
        "join_on",
        "SELECT true AS r FROM (VALUES (1)) a(x) JOIN (VALUES (1)) b(y) ON regexp_match('a', 'a') IS NOT NULL",
        false,
    ),
    (
        "window_frame",
        "SELECT sum(1) OVER (ROWS array_length(regexp_match('a', 'a'), 1) PRECEDING) > 0 AS r",
        false,
    ),
    (
        "where",
        "SELECT true AS r WHERE regexp_match('a', 'a') IS NOT NULL",
        false,
    ),
];

/// A view, a SQL-standard function and a rule action for every placement, each calling the scalar `regexp_match` where the stored form of an earlier release calls `regexp_matches`.
fn create_definitions(engine: &Engine) {
    sql(engine, "CREATE TABLE log (r boolean)");
    for (kind, query, _) in PLACEMENTS {
        sql(
            engine,
            &format!(
                "CREATE VIEW v_{kind} AS {query}; \
                 CREATE FUNCTION f_{kind}() RETURNS SETOF boolean LANGUAGE sql BEGIN ATOMIC {query}; END; \
                 CREATE TABLE e_{kind} (v integer); \
                 CREATE RULE r_{kind} AS ON INSERT TO e_{kind} DO ALSO INSERT INTO log {query}"
            ),
        );
    }
}

/// Rewrite the stored views, routines and rules into the form a release that resolved `regexp_matches` as a scalar function stored.
fn store_legacy_definitions(catalog: &dyn CatalogFacade) {
    let mut views = 0;
    for mut view in catalog.load_views().unwrap() {
        let (definition, renamed) = legacy_calls(&view.definition_json);
        if renamed > 0 {
            views += 1;
            view.definition_json = definition;
            catalog.save_view(&view).unwrap();
        }
    }
    assert_eq!(views, PLACEMENTS.len());
    for key in ["sql_functions_json", RULES_METADATA_KEY] {
        let stored = catalog.get_metadata(key).unwrap().unwrap();
        let (legacy, renamed) = if key == RULES_METADATA_KEY {
            legacy_rule_catalog(&stored)
        } else {
            legacy_calls(&stored)
        };
        assert!(renamed >= PLACEMENTS.len(), "{key}: {renamed}");
        catalog.set_metadata(key, &legacy).unwrap();
    }
}

fn count(engine: &Engine, statement: &str) -> uqa_core::Value {
    sql(engine, statement).rows[0]["n"].clone()
}

fn assert_set_valued(engine: &Engine, statement: &str) {
    let error = engine.sql(statement, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"), "{statement}: {error}");
    assert_eq!(
        error.to_string(),
        "set-valued function called in context that cannot accept a set",
        "{statement}"
    );
}

#[test]
fn legacy_set_returning_placements_load_and_keep_the_catalog_usable() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        create_definitions(&first);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        store_legacy_definitions(raw.catalog.as_ref());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        let placements = uqa_core::Value::Int(i64::try_from(PLACEMENTS.len()).unwrap());
        for statement in [
            "SELECT count(*) AS n FROM pg_class WHERE relkind = 'v' AND relname LIKE 'v\\_%'",
            "SELECT count(*) AS n FROM pg_attribute a JOIN pg_class c ON a.attrelid = c.oid WHERE c.relname LIKE 'v\\_%' AND a.attnum > 0",
            "SELECT count(*) AS n FROM information_schema.columns WHERE table_name LIKE 'v\\_%'",
            "SELECT count(*) AS n FROM pg_proc WHERE proname LIKE 'f\\_%'",
            "SELECT count(*) AS n FROM pg_rewrite WHERE rulename LIKE 'r\\_%'",
        ] {
            assert_eq!(count(&reopened, statement), placements, "{statement}");
        }
        for (kind, _, expands) in PLACEMENTS {
            for statement in [
                format!("SELECT * FROM v_{kind}"),
                format!("SELECT * FROM f_{kind}()"),
                format!("INSERT INTO e_{kind} VALUES (1)"),
            ] {
                if *expands {
                    sql(&reopened, &statement);
                } else {
                    assert_set_valued(&reopened, &statement);
                }
            }
        }
        // Each call the executor expanded produced one row from its one match.
        assert_eq!(
            count(&reopened, "SELECT count(*) AS n FROM log"),
            uqa_core::Value::Int(6)
        );
        sql(
            &reopened,
            "CREATE OR REPLACE VIEW v_and AS SELECT true AS r; SELECT * FROM v_and",
        );
        for (kind, _, _) in PLACEMENTS {
            sql(&reopened, &format!("DROP VIEW v_{kind}"));
        }
        assert_eq!(
            count(
                &reopened,
                "SELECT count(*) AS n FROM pg_class WHERE relname LIKE 'v\\_%'"
            ),
            uqa_core::Value::Int(0)
        );
    }
}
