//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn create_integer_overloads(engine: &Engine, name: &str) {
    for sql in [
        format!("CREATE FUNCTION {name}(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''int4'''"),
        format!("CREATE FUNCTION {name}(value BIGINT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''int8'''"),
    ] {
        engine.sql(&sql, &[]).unwrap();
    }
}

#[test]
fn scalar_sql_overloads_preserve_direct_and_subquery_declared_types() {
    let engine = Engine::new();
    create_integer_overloads(&engine, "scalar_pick");

    for (sql, expected) in [
        ("SELECT scalar_pick(7::INTEGER) AS v", "int4"),
        ("SELECT scalar_pick(7::BIGINT) AS v", "int8"),
        ("SELECT scalar_pick((SELECT 7::INTEGER)) AS v", "int4"),
        ("SELECT scalar_pick((SELECT 7::BIGINT)) AS v", "int8"),
    ] {
        assert_eq!(scalar(&engine, sql), Value::Str(expected.into()), "{sql}");
    }
}

#[test]
fn named_and_default_arguments_precede_search_path_shadowing() {
    let engine = Engine::new();
    for sql in [
        "CREATE SCHEMA first_choice",
        "CREATE SCHEMA second_choice",
        "CREATE FUNCTION first_choice.default_pick(value INTEGER, extra INTEGER DEFAULT 1) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''first'''",
        "CREATE FUNCTION second_choice.default_pick(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''second'''",
        "CREATE FUNCTION first_choice.named_pick(x INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''first'''",
        "CREATE FUNCTION second_choice.named_pick(y INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''second'''",
        "SET search_path = first_choice, second_choice, public",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    assert_eq!(
        scalar(&engine, "SELECT default_pick(1) AS v"),
        Value::Str("first".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT named_pick(x => 1) AS v"),
        Value::Str("first".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT named_pick(y => 1) AS v"),
        Value::Str("second".into())
    );

    engine
        .sql("SET search_path = second_choice, first_choice, public", &[])
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT default_pick(1) AS v"),
        Value::Str("second".into())
    );
}

#[test]
fn scalar_sql_overload_identity_survives_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("scalar-overloads.db");
    {
        let engine = Engine::open(&database).unwrap();
        create_integer_overloads(&engine, "persistent_pick");
        assert_eq!(
            scalar(&engine, "SELECT persistent_pick(7::BIGINT) AS v"),
            Value::Str("int8".into())
        );
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT persistent_pick(7::INTEGER) AS v"),
        Value::Str("int4".into())
    );
    assert_eq!(
        scalar(&reopened, "SELECT persistent_pick((SELECT 7::BIGINT)) AS v",),
        Value::Str("int8".into())
    );
    let routines = reopened
        .sql(
            "SELECT routine_type FROM information_schema.routines WHERE routine_name = 'persistent_pick'",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 2);
    assert!(routines
        .rows
        .iter()
        .all(|row| row["routine_type"] == Value::Str("FUNCTION".into())));
}

#[test]
fn information_schema_domain_overload_beats_base_and_survives_nested_casts() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE domain_carrier AS SELECT ordinal_position FROM information_schema.columns LIMIT 1",
        "CREATE FUNCTION domain_pick(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''base'''",
        "CREATE FUNCTION domain_pick(value domain_carrier.ordinal_position%TYPE) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''domain'''",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    assert_eq!(
        scalar(&engine, "SELECT domain_pick(1::INTEGER) AS v"),
        Value::Str("base".into())
    );
    for sql in [
        "SELECT domain_pick(1::information_schema.cardinal_number) AS v",
        "SELECT domain_pick((SELECT 1::information_schema.cardinal_number)) AS v",
        "SELECT domain_pick(((1::information_schema.cardinal_number)::INTEGER)::information_schema.cardinal_number) AS v",
    ] {
        assert_eq!(scalar(&engine, sql), Value::Str("domain".into()), "{sql}");
    }
}
