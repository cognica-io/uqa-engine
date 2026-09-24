//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual and persistent relations share SQL namespace ordering and retained bindings.

use super::relation_lock_support::{sessions, sql};
use super::*;

fn marker(engine: &Engine, query: &str, expected: &str) {
    let result = sql(engine, query);
    assert_eq!(result.rows.len(), 1, "{query}");
    assert_eq!(
        result.value_at(0, 0),
        Some(&Value::Str(expected.into())),
        "{query}"
    );
}

fn check_namespace(engine: &Engine) {
    sql(engine, "CREATE TABLE public.namespace_probe(v integer); CREATE TABLE public.pg_class(relname text); INSERT INTO public.pg_class VALUES ('public shadow'); CREATE TABLE public.tables(table_name text); INSERT INTO public.tables VALUES ('public tables')");
    marker(
        engine,
        "SELECT relname FROM pg_class WHERE relname = 'namespace_probe'",
        "namespace_probe",
    );
    assert_eq!(
        sql(engine, "SELECT 'pg_class'::regclass::oid AS oid").rows[0]["oid"],
        Value::Int(1259)
    );
    sql(engine, "SET search_path = public, pg_catalog");
    marker(engine, "SELECT relname FROM pg_class", "public shadow");
    sql(
        engine,
        "CREATE VIEW from_shadow AS SELECT relname FROM pg_class",
    );
    sql(engine, "SET search_path = public");
    marker(engine, "SELECT relname FROM from_shadow", "public shadow");
    sql(
        engine,
        "CREATE VIEW from_catalog AS SELECT relname FROM pg_class WHERE relname = 'namespace_probe'",
    );
    sql(engine, "CREATE TEMP TABLE pg_class(relname text); INSERT INTO pg_class VALUES ('temporary shadow')");
    marker(engine, "SELECT relname FROM pg_class", "temporary shadow");
    marker(
        engine,
        "SELECT relname FROM public.from_catalog",
        "namespace_probe",
    );
    sql(
        engine,
        "BEGIN; DECLARE named_source CURSOR FOR SELECT relname FROM pg_class",
    );
    sql(engine, "SET search_path = pg_catalog, public, pg_temp");
    marker(
        engine,
        "SELECT relname FROM pg_class WHERE relname = 'namespace_probe'",
        "namespace_probe",
    );
    marker(engine, "FETCH ALL FROM named_source", "temporary shadow");
    sql(
        engine,
        "COMMIT; SET search_path = information_schema, public",
    );
    marker(engine, "SELECT table_name FROM tables WHERE table_schema = 'public' AND table_name = 'from_catalog'", "from_catalog");
    sql(engine, "SET search_path = public, information_schema");
    marker(engine, "SELECT table_name FROM tables", "public tables");
}

#[test]
fn virtual_catalog_namespace_is_shared_by_queries_views_cursors_and_regclass() {
    check_namespace(&Engine::new());
    for provider in 0..3 {
        let (_directory, engine, _) = sessions(provider);
        check_namespace(&engine);
    }
}

#[test]
fn quoted_catalog_names_and_previously_unbound_catalog_views_are_consistent() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE public.\"PG_CLASS\"(value integer); INSERT INTO public.\"PG_CLASS\" VALUES(3)");
    assert_eq!(
        sql(&engine, "SELECT value FROM \"PG_CLASS\"").rows[0]["value"],
        Value::Int(3)
    );
    for name in [
        "pg_catalog.\"PG_CLASS\"",
        "\"PG_CATALOG\".pg_class",
        "\"pg_catalog.pg_class\"",
    ] {
        assert!(
            engine.sql(&format!("SELECT * FROM {name}"), &[]).is_err(),
            "{name}"
        );
        assert_eq!(
            sql(&engine, &format!("SELECT to_regclass('{name}') AS oid")).rows[0]["oid"],
            Value::Null
        );
    }
    for source in [
        "pg_range",
        "pg_auth_members",
        "pg_trigger",
        "pg_rewrite",
        "pg_rules",
        "pg_prepared_statements",
        "information_schema.information_schema_catalog_name",
    ] {
        sql(
            &engine,
            &format!("CREATE VIEW catalog_projection AS SELECT * FROM {source}"),
        );
        sql(&engine, "SELECT * FROM catalog_projection LIMIT 0");
        sql(&engine, "DROP VIEW catalog_projection");
    }
}

#[test]
fn physical_relations_named_like_session_catalogs_keep_their_mutation_behavior() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE TABLE public.pg_prepared_statements(v integer); SET search_path=public,pg_catalog",
    );
    for (query, expected) in [
        ("INSERT INTO pg_prepared_statements VALUES(1) RETURNING v", 1),
        ("UPDATE pg_prepared_statements SET v=v+1 RETURNING v", 2),
        ("WITH changed AS (UPDATE pg_prepared_statements SET v=v+1 RETURNING v) SELECT * FROM changed", 3),
        ("DELETE FROM pg_prepared_statements RETURNING v", 3),
    ] {
        assert_eq!(sql(&engine, query).rows[0]["v"], Value::Int(expected));
    }
}

#[test]
fn creating_an_existing_virtual_relation_does_not_publish_a_shadow_catalog_entry() {
    let engine = Engine::new();
    super::relation_lock_support::error(
        &engine,
        "CREATE TABLE pg_catalog.pg_class(v integer)",
        "42P07",
    );
    sql(
        &engine,
        "CREATE TABLE IF NOT EXISTS pg_catalog.pg_class(v integer)",
    );
    assert!(!engine
        .storage
        .tables
        .read()
        .contains_key(&RelationIdentity::new("pg_catalog", "pg_class")));
}
