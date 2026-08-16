//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine identity is `(schema, name, input types, kind)`, not name/arity.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;

fn scalar(engine: &Engine, sql: &str) -> Value {
    engine
        .sql(sql, &[])
        .unwrap()
        .rows
        .first()
        .and_then(|row| row.get("v"))
        .cloned()
        .unwrap()
}

fn create_typed_routines(engine: &Engine) {
    engine.sql("CREATE SCHEMA one", &[]).unwrap();
    engine.sql("CREATE SCHEMA two", &[]).unwrap();
    engine.sql("SET search_path TO one, public", &[]).unwrap();
    engine
        .sql(
            "CREATE FUNCTION pick(v int) RETURNS text AS $$
             BEGIN RETURN 'one-int'; END;
             $$ LANGUAGE plpgsql",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION one.pick(v text) RETURNS text AS $$
             BEGIN RETURN 'one-text'; END;
             $$ LANGUAGE plpgsql",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION two.pick(v int) RETURNS text AS $$
             BEGIN RETURN 'two-int'; END;
             $$ LANGUAGE plpgsql",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION one.coerce_only(v int) RETURNS int AS $$
             BEGIN RETURN v + 1; END;
             $$ LANGUAGE plpgsql",
            &[],
        )
        .unwrap();
}

fn assert_typed_routines(engine: &Engine) {
    assert_eq!(
        scalar(engine, "SELECT one.pick(7) AS v"),
        Value::Str("one-int".into())
    );
    assert_eq!(
        scalar(engine, "SELECT one.pick('x') AS v"),
        Value::Str("one-text-replaced".into())
    );
    assert_eq!(
        scalar(engine, "SELECT two.pick(7) AS v"),
        Value::Str("two-int".into())
    );

    engine.sql("SET search_path TO two, one", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT pick(7) AS v"),
        Value::Str("two-int".into())
    );
    engine.sql("SET search_path TO one, two", &[]).unwrap();
    assert_eq!(
        scalar(engine, "SELECT pick(7) AS v"),
        Value::Str("one-int".into())
    );
    // A sole compatible overload retains the existing implicit-coercion
    // behavior even though exact types now rank overloads.
    assert_eq!(
        scalar(engine, "SELECT coerce_only('7') AS v"),
        Value::Int(8)
    );
}

#[test]
fn qualified_user_table_routine_keeps_its_schema_identity() {
    let engine = Engine::new();
    engine.sql("CREATE SCHEMA application", &[]).unwrap();
    engine
        .sql(
            "CREATE FUNCTION application.rows_for(v int)
             RETURNS TABLE(x int) AS $$
             BEGIN RETURN QUERY SELECT v; END;
             $$ LANGUAGE plpgsql",
            &[],
        )
        .unwrap();

    let result = engine
        .sql("SELECT rows_for.x FROM application.rows_for(7)", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["x"], Value::Int(7));
    let scalar_subquery = engine
        .sql(
            "SELECT (SELECT rows_for.x FROM application.rows_for(7)) AS x",
            &[],
        )
        .unwrap();
    assert_eq!(scalar_subquery.rows[0]["x"], Value::Int(7));
    let wrong_schema = engine
        .sql("SELECT * FROM ag_catalog.rows_for(7)", &[])
        .unwrap_err();
    assert!(wrong_schema.to_string().contains("ag_catalog.rows_for"));
}

#[test]
fn schema_and_typed_overloads_are_distinct_and_survive_reopen() {
    let directory = TempDir::new().unwrap();
    let db = directory.path().join("routine-overloads.db");
    {
        let engine = Engine::open(&db).unwrap();
        create_typed_routines(&engine);

        let duplicate = engine
            .sql(
                "CREATE FUNCTION one.pick(v integer) RETURNS text AS $$
                 BEGIN RETURN 'duplicate'; END;
                 $$ LANGUAGE plpgsql",
                &[],
            )
            .unwrap_err();
        assert_eq!(duplicate.sqlstate(), Some("42723"));
        assert!(duplicate.to_string().contains("already exists"));

        engine
            .sql(
                "CREATE OR REPLACE FUNCTION one.pick(v text) RETURNS text AS $$
                 BEGIN RETURN 'one-text-replaced'; END;
                 $$ LANGUAGE plpgsql",
                &[],
            )
            .unwrap();
        assert_typed_routines(&engine);

        let routines = engine
            .sql(
                "SELECT routine_schema, routine_name, routine_type
                 FROM information_schema.routines
                 WHERE routine_name = 'pick'
                 ORDER BY routine_schema, specific_name",
                &[],
            )
            .unwrap();
        assert_eq!(routines.rows.len(), 3);
        assert_eq!(routines.rows[0]["routine_schema"], Value::Str("one".into()));
        assert_eq!(routines.rows[1]["routine_schema"], Value::Str("one".into()));
        assert_eq!(routines.rows[2]["routine_schema"], Value::Str("two".into()));
        assert!(routines
            .rows
            .iter()
            .all(|row| row["routine_type"] == Value::Str("FUNCTION".into())));

        let pg_proc = engine
            .sql(
                "SELECT proargtypes FROM pg_catalog.pg_proc
                 WHERE proname = 'pick'",
                &[],
            )
            .unwrap();
        let mut argument_oids = pg_proc
            .rows
            .iter()
            .map(|row| match &row["proargtypes"] {
                Value::List(values) => match values.as_slice() {
                    [Value::Int(oid)] => *oid,
                    other => panic!("unexpected proargtypes: {other:?}"),
                },
                other => panic!("unexpected proargtypes value: {other:?}"),
            })
            .collect::<Vec<_>>();
        argument_oids.sort_unstable();
        assert_eq!(argument_oids, vec![23, 23, 25]);
    }

    {
        let reopened = Engine::open(&db).unwrap();
        assert_typed_routines(&reopened);
        reopened.sql("DROP FUNCTION one.pick(text)", &[]).unwrap();
        assert_eq!(
            scalar(&reopened, "SELECT one.pick(7) AS v"),
            Value::Str("one-int".into())
        );
        let remaining = reopened
            .sql(
                "SELECT routine_name FROM information_schema.routines
                 WHERE routine_schema = 'one' AND routine_name = 'pick'",
                &[],
            )
            .unwrap();
        assert_eq!(remaining.rows.len(), 1);
    }

    let reopened_after_drop = Engine::open(&db).unwrap();
    assert_eq!(
        scalar(&reopened_after_drop, "SELECT one.pick(7) AS v"),
        Value::Str("one-int".into())
    );
    assert_eq!(
        scalar(&reopened_after_drop, "SELECT two.pick(7) AS v"),
        Value::Str("two-int".into())
    );
    let remaining = reopened_after_drop
        .sql(
            "SELECT routine_name FROM information_schema.routines
             WHERE routine_schema = 'one' AND routine_name = 'pick'",
            &[],
        )
        .unwrap();
    assert_eq!(remaining.rows.len(), 1);
}

fn assert_procedure_exists(engine: &Engine) {
    engine.sql("CALL owned.keep(1)", &[]).unwrap();
}

fn assert_function_exists(engine: &Engine) {
    assert_eq!(
        scalar(engine, "SELECT owned.keep_fn(1) AS v"),
        Value::Int(2)
    );
}

#[test]
fn wrong_kind_and_cascade_drops_are_atomic_and_schema_ownership_persists() {
    let directory = TempDir::new().unwrap();
    let db = directory.path().join("routine-drop.db");
    {
        let engine = Engine::open(&db).unwrap();
        engine.sql("CREATE SCHEMA owned", &[]).unwrap();
        engine
            .sql(
                "CREATE PROCEDURE owned.keep(v int) AS $$
                 BEGIN NULL; END;
                 $$ LANGUAGE plpgsql",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE FUNCTION owned.keep_fn(v int) RETURNS int AS $$
                 BEGIN RETURN v + 1; END;
                 $$ LANGUAGE plpgsql",
                &[],
            )
            .unwrap();

        let wrong_function_kind = engine
            .sql("DROP FUNCTION owned.keep(int)", &[])
            .unwrap_err();
        assert_eq!(wrong_function_kind.sqlstate(), Some("42809"));
        assert_procedure_exists(&engine);

        let wrong_procedure_kind = engine
            .sql("DROP PROCEDURE owned.keep_fn(int)", &[])
            .unwrap_err();
        assert_eq!(wrong_procedure_kind.sqlstate(), Some("42809"));
        assert_function_exists(&engine);

        let collision = engine
            .sql(
                "CREATE OR REPLACE FUNCTION owned.keep(v int) RETURNS int AS $$
                 BEGIN RETURN v; END;
                 $$ LANGUAGE plpgsql",
                &[],
            )
            .unwrap_err();
        assert!(collision.to_string().contains("cannot change routine kind"));
        assert_procedure_exists(&engine);

        let cascade = engine
            .sql("DROP PROCEDURE owned.keep(int) CASCADE", &[])
            .unwrap_err();
        assert!(
            cascade.to_string().contains("CASCADE")
                && cascade.to_string().contains("not supported"),
            "unexpected DROP CASCADE error: {cascade}"
        );
        assert_procedure_exists(&engine);

        let nonempty_schema = engine.sql("DROP SCHEMA owned", &[]).unwrap_err();
        assert!(nonempty_schema.to_string().contains("not empty"));
        assert!(engine.has_schema("owned").unwrap());
    }

    {
        let engine = Engine::open(&db).unwrap();
        assert_procedure_exists(&engine);
        assert_function_exists(&engine);
        let nonempty_schema = engine.sql("DROP SCHEMA owned", &[]).unwrap_err();
        assert!(nonempty_schema.to_string().contains("not empty"));

        engine
            .sql("DROP PROCEDURE owned.keep(integer)", &[])
            .unwrap();
        engine
            .sql("DROP FUNCTION owned.keep_fn(integer)", &[])
            .unwrap();
        engine.sql("DROP SCHEMA owned", &[]).unwrap();
    }

    let reopened = Engine::open(&db).unwrap();
    assert!(!reopened.has_schema("owned").unwrap());
}
