//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine lookup uses `(schema, name, input types)`, while durable object identity survives replacement and rename and call shape and expected kind are validated separately.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};
use uqa_sql::ast::{Expr, FunctionBinding};
use uqa_sql::Statement;

#[path = "sql_routine_identity/alter_routine.rs"]
mod alter_routine;
#[path = "sql_routine_identity/polymorphic_variadic.rs"]
mod polymorphic_variadic;
#[path = "sql_routine_identity/routine_cascade.rs"]
mod routine_cascade;
#[path = "sql_routine_identity/routine_rename.rs"]
mod routine_rename;
#[path = "sql_routine_identity/scalar_overloads.rs"]
mod scalar_overloads;
#[path = "sql_routine_identity/security_roles.rs"]
mod security_roles;
#[path = "sql_routine_identity/trigger_when_dependencies.rs"]
mod trigger_when_dependencies;

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
fn table_routine_overload_uses_scalar_subquery_declared_type() {
    let engine = Engine::new();
    for sql in [
        "CREATE FUNCTION table_pick(value INTEGER) RETURNS TABLE(int4_value TEXT) AS $$ BEGIN RETURN QUERY SELECT 'int4'; END; $$ LANGUAGE plpgsql",
        "CREATE FUNCTION table_pick(value BIGINT) RETURNS TABLE(int8_value TEXT) AS $$ BEGIN RETURN QUERY SELECT 'int8'; END; $$ LANGUAGE plpgsql",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    assert_eq!(
        scalar(
            &engine,
            "SELECT int8_value AS v FROM table_pick((SELECT 7::BIGINT))",
        ),
        Value::Str("int8".into())
    );
}

#[test]
fn table_routine_unknown_arguments_use_postgresql_categories_and_stable_binding() {
    let engine = Engine::new();
    for sql in [
        "CREATE FUNCTION uuid_table(value UUID) RETURNS TABLE(chosen TEXT) AS $$ BEGIN RETURN QUERY SELECT 'uuid'; END; $$ LANGUAGE plpgsql",
        "CREATE FUNCTION category_table(value TEXT) RETURNS TABLE(chosen TEXT) AS $$ BEGIN RETURN QUERY SELECT 'text'; END; $$ LANGUAGE plpgsql",
        "CREATE FUNCTION category_table(value BYTEA) RETURNS TABLE(chosen TEXT) AS $$ BEGIN RETURN QUERY SELECT 'bytea'; END; $$ LANGUAGE plpgsql",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    assert_eq!(
        scalar(
            &engine,
            "SELECT chosen AS v FROM uuid_table('00000000-0000-0000-0000-000000000001')",
        ),
        Value::Str("uuid".into())
    );
    let parameter = engine
        .sql(
            "SELECT chosen AS v FROM uuid_table($1)",
            &[SQLParam::Scalar(Value::Str(
                "00000000-0000-0000-0000-000000000001".into(),
            ))],
        )
        .unwrap();
    assert_eq!(parameter.rows[0]["v"], Value::Str("uuid".into()));
    assert_eq!(
        scalar(
            &engine,
            "SELECT chosen AS v FROM category_table('plain text')",
        ),
        Value::Str("text".into())
    );
}

#[test]
fn table_routine_smallint_call_is_ambiguous_between_int4_and_int8() {
    let engine = Engine::new();
    for sql in [
        "CREATE FUNCTION table_small(value INTEGER) RETURNS TABLE(chosen TEXT) AS $$ BEGIN RETURN QUERY SELECT 'int4'; END; $$ LANGUAGE plpgsql",
        "CREATE FUNCTION table_small(value BIGINT) RETURNS TABLE(chosen TEXT) AS $$ BEGIN RETURN QUERY SELECT 'int8'; END; $$ LANGUAGE plpgsql",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    let error = engine
        .sql("SELECT * FROM table_small(1::SMALLINT)", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42725"), "{error}");
}

#[test]
fn table_routine_overload_prefers_exact_percent_type_domain() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE domain_table_carrier AS SELECT ordinal_position FROM information_schema.columns LIMIT 1",
        "CREATE FUNCTION domain_table(value INTEGER) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''base'''",
        "CREATE FUNCTION domain_table(value domain_table_carrier.ordinal_position%TYPE) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''domain'''",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    assert_eq!(
        scalar(
            &engine,
            "SELECT chosen AS v FROM domain_table(1::information_schema.cardinal_number)",
        ),
        Value::Str("domain".into())
    );
}

#[test]
fn table_routine_named_and_default_arguments_precede_search_path_shadowing() {
    let engine = Engine::new();
    for sql in [
        "CREATE SCHEMA table_first",
        "CREATE SCHEMA table_second",
        "CREATE FUNCTION table_first.default_table(value INTEGER, extra INTEGER DEFAULT 1) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''first'''",
        "CREATE FUNCTION table_second.default_table(value INTEGER) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''second'''",
        "CREATE FUNCTION table_first.named_table(x INTEGER) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''first'''",
        "CREATE FUNCTION table_second.named_table(y INTEGER) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''second'''",
        "SET search_path = table_first, table_second, public",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    assert_eq!(
        scalar(&engine, "SELECT chosen AS v FROM default_table(1)"),
        Value::Str("first".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT chosen AS v FROM named_table(x => 1)"),
        Value::Str("first".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT chosen AS v FROM named_table(y => 1)"),
        Value::Str("second".into())
    );

    engine
        .sql("SET search_path = table_second, table_first, public", &[])
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT chosen AS v FROM default_table(1)"),
        Value::Str("second".into())
    );
}

#[test]
fn table_routine_overload_identity_survives_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("table-overloads.db");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE FUNCTION persistent_table(value INTEGER) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''int4'''",
            "CREATE FUNCTION persistent_table(value BIGINT) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''int8'''",
        ] {
            engine.sql(sql, &[]).unwrap();
        }
        assert_eq!(
            scalar(
                &engine,
                "SELECT chosen AS v FROM persistent_table(7::BIGINT)",
            ),
            Value::Str("int8".into())
        );
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT chosen AS v FROM persistent_table(7::INTEGER)",
        ),
        Value::Str("int4".into())
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT chosen AS v FROM persistent_table((SELECT 7::BIGINT))",
        ),
        Value::Str("int8".into())
    );
}

#[test]
fn table_routine_view_binding_survives_search_path_changes_and_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("table-view-binding.db");
    {
        let engine = Engine::open(&database).unwrap();
        for sql in [
            "CREATE SCHEMA table_view_first",
            "CREATE SCHEMA table_view_second",
            "CREATE FUNCTION table_view_first.bound_table(value BIGINT) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''first'''",
            "CREATE FUNCTION table_view_first.bound_table(value INTEGER) RETURNS TABLE(unused TEXT) LANGUAGE SQL AS 'SELECT ''unused'''",
            "CREATE FUNCTION table_view_second.bound_table(value BIGINT) RETURNS TABLE(other TEXT) LANGUAGE SQL AS 'SELECT ''second'''",
            "SET search_path = table_view_first, table_view_second, public",
            "CREATE VIEW bound_table_view AS SELECT chosen FROM bound_table(7::BIGINT)",
            "SET search_path = table_view_second, table_view_first, public",
        ] {
            engine.sql(sql, &[]).unwrap();
        }
        assert_eq!(
            scalar(
                &engine,
                "SELECT chosen AS v FROM table_view_first.bound_table_view",
            ),
            Value::Str("first".into())
        );
    }

    let reopened = Engine::open(&database).unwrap();
    reopened
        .sql(
            "SET search_path = table_view_second, table_view_first, public",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT chosen AS v FROM table_view_first.bound_table_view",
        ),
        Value::Str("first".into())
    );

    reopened
        .sql("DROP FUNCTION table_view_first.bound_table(INTEGER)", &[])
        .unwrap();
    reopened
        .sql("DROP FUNCTION table_view_second.bound_table(BIGINT)", &[])
        .unwrap();
    let dependency = reopened
        .sql("DROP FUNCTION table_view_first.bound_table(BIGINT)", &[])
        .unwrap_err();
    assert_eq!(dependency.sqlstate(), Some("2BP01"), "{dependency}");
    assert!(
        dependency.to_string().contains("bound_table_view"),
        "{dependency}"
    );
    assert_eq!(
        scalar(
            &reopened,
            "SELECT chosen AS v FROM table_view_first.bound_table_view",
        ),
        Value::Str("first".into())
    );
    let remaining = reopened
        .sql(
            "SELECT routine_name AS v FROM information_schema.routines WHERE routine_schema = 'table_view_first' AND routine_name = 'bound_table'",
            &[],
        )
        .unwrap();
    assert_eq!(remaining.rows.len(), 1);
}

#[test]
fn scalar_view_function_dependencies_are_exact_replaceable_and_drop_atomic() {
    let engine = Engine::new();
    for sql in [
        "CREATE SCHEMA view_dep",
        "SET search_path = view_dep, pg_catalog, public",
        "CREATE FUNCTION chosen(value INTEGER) RETURNS TEXT LANGUAGE SQL AS 'SELECT ''chosen'''",
        "CREATE FUNCTION chosen(value BIGINT) RETURNS TEXT LANGUAGE SQL AS 'SELECT ''other-overload'''",
        "CREATE FUNCTION replacement(value INTEGER) RETURNS TEXT LANGUAGE SQL AS 'SELECT ''replacement'''",
        "CREATE FUNCTION free_function(value INTEGER) RETURNS TEXT LANGUAGE SQL AS 'SELECT ''free'''",
        "CREATE VIEW exact_scalar_view AS SELECT chosen(1::INTEGER) AS value",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    engine
        .sql("DROP FUNCTION view_dep.chosen(BIGINT)", &[])
        .unwrap();
    let atomic = engine
        .sql(
            "DROP FUNCTION view_dep.free_function(INTEGER), view_dep.chosen(INTEGER)",
            &[],
        )
        .unwrap_err();
    assert_eq!(atomic.sqlstate(), Some("2BP01"), "{atomic}");
    assert_eq!(
        scalar(&engine, "SELECT view_dep.free_function(1) AS v"),
        Value::Str("free".into())
    );
    assert_eq!(
        scalar(&engine, "SELECT value AS v FROM view_dep.exact_scalar_view"),
        Value::Str("chosen".into())
    );

    engine
        .sql(
            "CREATE OR REPLACE VIEW view_dep.exact_scalar_view AS SELECT view_dep.replacement(1::INTEGER) AS value",
            &[],
        )
        .unwrap();
    engine
        .sql("DROP FUNCTION view_dep.chosen(INTEGER)", &[])
        .unwrap();
    let replacement_dependency = engine
        .sql("DROP FUNCTION view_dep.replacement(INTEGER)", &[])
        .unwrap_err();
    assert_eq!(
        replacement_dependency.sqlstate(),
        Some("2BP01"),
        "{replacement_dependency}"
    );
    assert_eq!(
        scalar(&engine, "SELECT value AS v FROM view_dep.exact_scalar_view"),
        Value::Str("replacement".into())
    );

    for sql in [
        "CREATE FUNCTION view_dep.shared_dependency(value INTEGER) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''shared'''",
        "CREATE TABLE view_dep.generated_dependency (source INTEGER, derived TEXT GENERATED ALWAYS AS (view_dep.shared_dependency(source)) STORED)",
        "CREATE VIEW view_dep.function_dependency AS SELECT view_dep.shared_dependency(1::INTEGER) AS value",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    let combined_dependency = engine
        .sql("DROP FUNCTION view_dep.shared_dependency(INTEGER)", &[])
        .unwrap_err();
    assert_eq!(
        combined_dependency.sqlstate(),
        Some("2BP01"),
        "{combined_dependency}"
    );
    assert!(
        combined_dependency
            .to_string()
            .contains("generated_dependency.derived"),
        "{combined_dependency}"
    );
    assert!(
        combined_dependency
            .to_string()
            .contains("function_dependency"),
        "{combined_dependency}"
    );

    engine
        .sql(
            "CREATE VIEW view_dep.builtin_scalar_view AS SELECT pg_catalog.lower('ABC') AS value",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION view_dep.lower(value TEXT) RETURNS TEXT LANGUAGE SQL AS 'SELECT ''user-lower'''",
            &[],
        )
        .unwrap();
    engine
        .sql("DROP FUNCTION view_dep.lower(TEXT)", &[])
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT value AS v FROM view_dep.builtin_scalar_view",
        ),
        Value::Str("abc".into())
    );
}

#[test]
fn view_function_dependency_scan_covers_function_groups_and_nested_query_shapes() {
    let engine = Engine::new();
    for sql in [
        "CREATE SCHEMA structural_dep",
        "SET search_path = structural_dep, pg_catalog, public",
        "CREATE FUNCTION group_left(value INTEGER) RETURNS TABLE(left_value INTEGER) LANGUAGE SQL AS 'SELECT $1'",
        "CREATE FUNCTION group_right(value INTEGER) RETURNS TABLE(right_value INTEGER) LANGUAGE SQL AS 'SELECT $1'",
        "CREATE FUNCTION nested_scalar(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT $1 + 10'",
        "CREATE VIEW function_group_view AS SELECT left_value, right_value FROM ROWS FROM (group_left(1), group_right(2)) AS grouped(left_value, right_value)",
        "CREATE VIEW nested_plan_view AS WITH source AS (SELECT left_value AS value FROM ROWS FROM (group_left(1)) AS grouped(left_value)) SELECT nested_scalar(derived.value) AS value FROM (SELECT value FROM source) AS derived JOIN (VALUES (1)) AS marker(id) ON true UNION ALL SELECT (SELECT nested_scalar(nested.v) FROM (VALUES (2)) AS nested(v)) AS value",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    assert_eq!(
        scalar(
            &engine,
            "SELECT value AS v FROM structural_dep.nested_plan_view ORDER BY value LIMIT 1",
        ),
        Value::Int(11)
    );
    for (signature, expected_view) in [
        ("structural_dep.group_left(INTEGER)", "function_group_view"),
        ("structural_dep.group_right(INTEGER)", "function_group_view"),
        ("structural_dep.nested_scalar(INTEGER)", "nested_plan_view"),
    ] {
        let dependency = engine
            .sql(&format!("DROP FUNCTION {signature}"), &[])
            .unwrap_err();
        assert_eq!(dependency.sqlstate(), Some("2BP01"), "{dependency}");
        assert!(
            dependency.to_string().contains(expected_view),
            "{dependency}"
        );
    }
}

#[test]
fn multi_argument_from_unnest_uses_postgresql_syntax_before_user_overloads() {
    let engine = Engine::new();
    for sql in [
        "CREATE SCHEMA user_table_api",
        "CREATE FUNCTION user_table_api.unnest(left_values INTEGER[], right_values INTEGER[]) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''user-unnest'''",
        "SET search_path = user_table_api, pg_catalog, public",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    let builtin = engine
        .sql(
            "SELECT left_value, right_value
             FROM unnest(ARRAY[1, 3]::INTEGER[], ARRAY[2]::INTEGER[])
                  AS u(left_value, right_value)",
            &[],
        )
        .unwrap();
    assert_eq!(builtin.rows.len(), 2);
    assert_eq!(builtin.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(builtin.value_at(0, 1), Some(&Value::Int(2)));
    assert_eq!(builtin.value_at(1, 0), Some(&Value::Int(3)));
    assert_eq!(builtin.value_at(1, 1), Some(&Value::Null));

    assert_eq!(
        scalar(
            &engine,
            "SELECT chosen AS v
             FROM user_table_api.unnest(ARRAY[1]::INTEGER[], ARRAY[2]::INTEGER[])",
        ),
        Value::Str("user-unnest".into())
    );

    let qualified_builtin = engine
        .sql(
            "SELECT *
             FROM pg_catalog.unnest(ARRAY[1]::INTEGER[], ARRAY[2]::INTEGER[])",
            &[],
        )
        .unwrap_err();
    assert_eq!(qualified_builtin.sqlstate(), Some("42883"));

    assert_eq!(
        scalar(
            &engine,
            "SELECT unnest(ARRAY[1]::INTEGER[], ARRAY[2]::INTEGER[]) AS v",
        ),
        Value::Str("user-unnest".into())
    );
}

#[test]
fn builtin_set_projection_binding_survives_same_named_user_routine_family() {
    let engine = Engine::new();
    engine.sql("CREATE SCHEMA projection_api", &[]).unwrap();
    let mut statements = uqa_sql::compile("SELECT generate_series(1, 3) AS value").unwrap();
    let Statement::Select(mut body) = statements.remove(0) else {
        panic!("expected SELECT statement");
    };
    let Expr::Func { binding, .. } = &mut body.projections[0].expr else {
        panic!("expected function projection");
    };
    *binding = Some(FunctionBinding {
        object_id: None,
        name: "pg_catalog.generate_series".into(),
        argument_types: vec!["integer".into(), "integer".into()],
        builtin: true,
        dispatch: None,
        invocation: None,
        resolution_error: None,
    });
    engine
        .register_view("projection_api.builtin_series", *body)
        .unwrap();
    for sql in [
        "CREATE FUNCTION projection_api.generate_series(first_value TEXT, last_value TEXT) RETURNS SETOF TEXT LANGUAGE SQL AS 'SELECT $1'",
        "SET search_path = pg_catalog, projection_api, public",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    let result = engine
        .sql("SELECT value FROM projection_api.builtin_series", &[])
        .unwrap();
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row["value"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2), Value::Int(3)]
    );
}

#[test]
fn table_routine_exact_overload_binding_reaches_dml_and_correlated_lateral_sources() {
    let engine = Engine::new();
    for sql in [
        "CREATE FUNCTION table_source_pick(value INTEGER) RETURNS TABLE(int4_value TEXT) LANGUAGE SQL AS 'SELECT ''int4'''",
        "CREATE FUNCTION table_source_pick(value BIGINT) RETURNS TABLE(int8_value TEXT) LANGUAGE SQL AS 'SELECT ''int8'''",
        "CREATE TABLE table_binding_target (id INTEGER PRIMARY KEY, chosen TEXT)",
        "CREATE TABLE table_binding_input (value BIGINT)",
        "INSERT INTO table_binding_target VALUES (1, 'old'), (2, 'old'), (3, 'old')",
        "INSERT INTO table_binding_input VALUES (7)",
    ] {
        engine.sql(sql, &[]).unwrap();
    }

    let updated = engine
        .sql(
            "UPDATE table_binding_target AS target
             SET chosen = source.int8_value
             FROM table_source_pick((SELECT 7::BIGINT)) AS source
             WHERE target.id = 1
             RETURNING target.chosen AS v",
            &[],
        )
        .unwrap();
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(updated.rows[0]["v"], Value::Str("int8".into()));

    let deleted = engine
        .sql(
            "DELETE FROM table_binding_target AS target
             USING table_source_pick((SELECT 7::BIGINT)) AS source
             WHERE target.id = 2 AND source.int8_value = 'int8'
             RETURNING source.int8_value AS v",
            &[],
        )
        .unwrap();
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(deleted.rows[0]["v"], Value::Str("int8".into()));

    let merged = engine
        .sql(
            "MERGE INTO table_binding_target AS target
             USING table_source_pick((SELECT 7::BIGINT)) AS source
             ON target.id = 3
             WHEN MATCHED THEN UPDATE SET chosen = source.int8_value
             RETURNING target.chosen AS v",
            &[],
        )
        .unwrap();
    assert_eq!(merged.affected_rows, 1);
    assert_eq!(merged.rows[0]["v"], Value::Str("int8".into()));

    assert_eq!(
        scalar(
            &engine,
            "SELECT picked.int8_value AS v
             FROM table_binding_input AS input
             CROSS JOIN LATERAL (
                 SELECT int8_value
                 FROM table_source_pick(input.value)
             ) AS picked",
        ),
        Value::Str("int8".into())
    );
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

        engine
            .sql("DROP PROCEDURE owned.keep(int) CASCADE", &[])
            .unwrap();
        let missing = engine.sql("CALL owned.keep(1)", &[]).unwrap_err();
        assert_eq!(missing.sqlstate(), Some("42883"), "{missing}");

        let nonempty_schema = engine.sql("DROP SCHEMA owned", &[]).unwrap_err();
        assert!(nonempty_schema.to_string().contains("not empty"));
        assert!(engine.has_schema("owned").unwrap());
    }

    {
        let engine = Engine::open(&db).unwrap();
        let missing = engine.sql("CALL owned.keep(1)", &[]).unwrap_err();
        assert_eq!(missing.sqlstate(), Some("42883"), "{missing}");
        assert_function_exists(&engine);
        let nonempty_schema = engine.sql("DROP SCHEMA owned", &[]).unwrap_err();
        assert!(nonempty_schema.to_string().contains("not empty"));

        engine
            .sql("DROP FUNCTION owned.keep_fn(integer)", &[])
            .unwrap();
        engine.sql("DROP SCHEMA owned", &[]).unwrap();
    }

    let reopened = Engine::open(&db).unwrap();
    assert!(!reopened.has_schema("owned").unwrap());
}
