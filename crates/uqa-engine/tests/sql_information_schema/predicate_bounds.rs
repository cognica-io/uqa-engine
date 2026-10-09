//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, Value};

fn count(engine: &Engine, predicate: &str) -> i64 {
    let query = format!("SELECT count(*) AS n FROM information_schema.columns c WHERE {predicate}");
    let result = engine.sql(&query, &[]).unwrap();
    let Value::Int(n) = result.rows[0]["n"] else {
        panic!("count");
    };
    n
}

#[test]
fn catalog_name_bounds_preserve_residual_filters_aliases_and_outer_joins() {
    let engine = Engine::new();
    engine.sql("CREATE SCHEMA hidden_bounds; CREATE TABLE bound_a(id integer, type_ref regtype DEFAULT 'text'::regtype); CREATE TABLE bound_b(id integer, type_ref regtype DEFAULT 'name'::regtype); CREATE TABLE hidden_bounds.bound_a(id integer); CREATE VIEW bound_v AS SELECT type_ref FROM bound_a", &[]).unwrap();
    for (predicate, expected) in [
        ("c.table_schema='public' AND c.table_name='bound_a'", 2),
        ("c.table_schema='public' AND 'bound_a'=c.table_name", 2),
        (
            "c.table_schema='hidden_bounds' AND c.table_name='bound_a'",
            1,
        ),
        (
            "c.table_schema='public' AND (c.table_name='bound_a' OR c.table_name='bound_b')",
            4,
        ),
        (
            "c.table_schema='public' AND c.table_name::text='bound_a'",
            2,
        ),
        ("c.table_name='bound_a' AND c.table_name='bound_b'", 0),
        ("c.table_name='missing_bound_table'", 0),
        (
            "c.table_schema='public' AND c.table_name='bound_a' AND c.column_default IS NOT NULL",
            1,
        ),
        ("c.table_schema='public' AND c.table_name='bound_v'", 1),
    ] {
        assert_eq!(count(&engine, predicate), expected, "{predicate}");
    }
    let aliases = engine.sql("SELECT c.position,c.definition FROM information_schema.columns c(cat,schema_name,relation,attribute,position,definition) WHERE c.schema_name='public' AND c.relation='bound_a' AND c.attribute='type_ref'", &[]).unwrap();
    assert_eq!(aliases.rows[0]["position"], Value::Int(2));
    assert_eq!(
        aliases.rows[0]["definition"],
        Value::Str("'text'::regtype".into())
    );
    let parameterized = engine.sql("SELECT column_name FROM information_schema.columns WHERE table_schema='public' AND table_name=$1 ORDER BY ordinal_position", &[uqa_sql::SQLParam::Scalar(Value::Str("bound_a".into()))]).unwrap();
    assert_eq!(parameterized.rows.len(), 2);
    let joined = engine.sql("SELECT v.name,c.column_default FROM (VALUES ('bound_a'),('missing_bound_table')) v(name) LEFT JOIN information_schema.columns c ON c.table_name=v.name AND c.table_schema='public' AND c.column_name='type_ref' ORDER BY v.name", &[]).unwrap();
    assert_eq!(joined.rows.len(), 2);
    assert_eq!(
        joined.rows[0]["column_default"],
        Value::Str("'text'::regtype".into())
    );
    assert_eq!(joined.rows[1]["column_default"], Value::Null);
    engine
        .sql("BEGIN; ALTER TABLE bound_a RENAME TO bound_renamed", &[])
        .unwrap();
    assert_eq!(
        count(
            &engine,
            "c.table_schema='public' AND c.table_name='bound_a'"
        ),
        0
    );
    assert_eq!(
        count(
            &engine,
            "c.table_schema='public' AND c.table_name='bound_renamed'"
        ),
        2
    );
    engine.sql("ROLLBACK; CREATE ROLE bound_reader; GRANT SELECT(type_ref) ON bound_a TO bound_reader; SET ROLE bound_reader", &[]).unwrap();
    assert_eq!(
        count(
            &engine,
            "c.table_schema='public' AND c.table_name='bound_a'"
        ),
        1
    );
    assert_eq!(count(&engine, "c.table_name='bound_b'"), 0);
    engine.sql("RESET ROLE", &[]).unwrap();
    assert_eq!(
        count(
            &engine,
            "c.table_schema='public' AND c.table_name='bound_a'"
        ),
        2
    );
}
