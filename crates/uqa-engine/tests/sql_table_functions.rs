//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table functions in `FROM`, standalone `VALUES`, and scalar table-function
//! bodies, including series generation, unnesting, regular-expression splits,
//! and JSON object or array expansion.

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ast::ColumnType;

fn values(result: &uqa_engine::SQLResult, column: &str) -> Vec<Value> {
    result.rows.iter().map(|row| row[column].clone()).collect()
}

fn assert_sql_error(eng: &Engine, sql: &str, sqlstate: &str, message: &str) {
    let error = eng.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}");
    assert_eq!(error.to_string(), message, "{sql}");
}

#[test]
fn generate_series_basic() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(1, 5) AS t(n)", &[])
        .unwrap();
    assert_eq!(
        values(&r, "n"),
        vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5)
        ]
    );
}

#[test]
fn pg_catalog_qualified_generate_series_uses_the_builtin() {
    let eng = Engine::new();
    let result = eng
        .sql(
            "SELECT n FROM pg_catalog.generate_series(1, 3) AS t(n)",
            &[],
        )
        .unwrap();
    assert_eq!(
        values(&result, "n"),
        vec![Value::Int(1), Value::Int(2), Value::Int(3)]
    );
}

#[test]
fn table_function_scalar_subqueries_preserve_declared_argument_types() {
    let eng = Engine::new();
    let result = eng
        .sql(
            "SELECT value FROM generate_series((SELECT 1::BIGINT), (SELECT 2::BIGINT)) AS g(value)",
            &[],
        )
        .unwrap();
    assert_eq!(result.column_types, [Some(ColumnType::BigInteger)]);
    assert_eq!(values(&result, "value"), vec![Value::Int(1), Value::Int(2)]);
}

#[test]
fn table_function_default_column_keeps_the_local_identifier_structured() {
    let eng = Engine::new();
    let builtin = eng
        .sql(
            "SELECT generate_series FROM pg_catalog.generate_series(1, 2)",
            &[],
        )
        .unwrap();
    assert_eq!(
        values(&builtin, "generate_series"),
        vec![Value::Int(1), Value::Int(2)]
    );

    eng.sql(
        "CREATE FUNCTION \"series.dot\"(n integer) RETURNS SETOF integer AS $$
           SELECT generate_series(1, n)
         $$ LANGUAGE sql",
        &[],
    )
    .unwrap();
    let quoted = eng
        .sql("SELECT \"series.dot\" FROM \"series.dot\"(2)", &[])
        .unwrap();
    assert_eq!(
        values(&quoted, "series.dot"),
        vec![Value::Int(1), Value::Int(2)]
    );
}

#[test]
fn generate_series_relation_alias_is_default_column_alias() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT x FROM generate_series(1, 3) AS x", &[])
        .unwrap();
    assert_eq!(
        values(&r, "x"),
        vec![Value::Int(1), Value::Int(2), Value::Int(3)]
    );
}

#[test]
fn table_functions_with_ordinality_append_a_typed_aliased_column() {
    let eng = Engine::new();
    let default_names = eng
        .sql("SELECT * FROM generate_series(4, 6) WITH ORDINALITY", &[])
        .unwrap();
    assert_eq!(default_names.columns, ["generate_series", "ordinality"]);
    assert_eq!(
        default_names.column_types,
        [Some(ColumnType::Integer), Some(ColumnType::BigInteger)]
    );
    assert_eq!(default_names.value_at(0, 0), Some(&Value::Int(4)));
    assert_eq!(default_names.value_at(0, 1), Some(&Value::Int(1)));
    assert_eq!(default_names.value_at(2, 1), Some(&Value::Int(3)));

    let explicit_aliases = eng
        .sql(
            "SELECT g.value, g.sequence \
             FROM generate_series(4, 5) WITH ORDINALITY AS g(value, sequence) \
             ORDER BY g.sequence",
            &[],
        )
        .unwrap();
    assert_eq!(explicit_aliases.columns, ["value", "sequence"]);
    assert_eq!(explicit_aliases.value_at(1, 0), Some(&Value::Int(5)));
    assert_eq!(explicit_aliases.value_at(1, 1), Some(&Value::Int(2)));

    let partial_alias = eng
        .sql(
            "SELECT * FROM generate_series(4, 4) WITH ORDINALITY AS g(value)",
            &[],
        )
        .unwrap();
    assert_eq!(partial_alias.columns, ["value", "ordinality"]);

    let empty = eng
        .sql(
            "SELECT * FROM generate_series(5, 1) WITH ORDINALITY AS g(value, sequence)",
            &[],
        )
        .unwrap();
    assert_eq!(empty.columns, ["value", "sequence"]);
    assert_eq!(
        empty.column_types,
        [Some(ColumnType::Integer), Some(ColumnType::BigInteger)]
    );
    assert!(empty.rows.is_empty());

    let error = eng
        .sql(
            "SELECT * FROM generate_series(1, 1) WITH ORDINALITY AS g(a, b, c)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P10"));
}

#[test]
fn table_function_ordinality_resets_for_each_lateral_invocation() {
    let eng = Engine::new();
    let result = eng
        .sql(
            "SELECT v.n, g.value, g.ordinality \
             FROM (VALUES (2), (0), (1)) AS v(n) \
             CROSS JOIN LATERAL generate_series(1, v.n) \
             WITH ORDINALITY AS g(value, ordinality) \
             ORDER BY v.n DESC, g.ordinality",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(2)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(1)));
    assert_eq!(result.value_at(0, 2), Some(&Value::Int(1)));
    assert_eq!(result.value_at(1, 1), Some(&Value::Int(2)));
    assert_eq!(result.value_at(1, 2), Some(&Value::Int(2)));
    assert_eq!(result.value_at(2, 0), Some(&Value::Int(1)));
    assert_eq!(result.value_at(2, 2), Some(&Value::Int(1)));
}

#[test]
fn multi_column_table_functions_put_ordinality_last() {
    let eng = Engine::new();
    let unnested = eng
        .sql(
            "SELECT * \
             FROM unnest(ARRAY[1, 2], ARRAY['x']) WITH ORDINALITY AS u(a, b, n)",
            &[],
        )
        .unwrap();
    assert_eq!(unnested.columns, ["a", "b", "n"]);
    assert_eq!(
        unnested.column_types,
        [
            Some(ColumnType::Integer),
            Some(ColumnType::Text),
            Some(ColumnType::BigInteger),
        ]
    );
    assert_eq!(unnested.value_at(1, 0), Some(&Value::Int(2)));
    assert_eq!(unnested.value_at(1, 1), Some(&Value::Null));
    assert_eq!(unnested.value_at(1, 2), Some(&Value::Int(2)));

    let json = eng
        .sql(
            "SELECT * FROM json_each('{\"a\": 1}') WITH ORDINALITY AS j(k)",
            &[],
        )
        .unwrap();
    assert_eq!(json.columns, ["k", "value", "ordinality"]);
    assert_eq!(json.value_at(0, 2), Some(&Value::Int(1)));
}

#[test]
fn generate_series_with_step() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(0, 10, 3) AS t(n)", &[])
        .unwrap();
    assert_eq!(
        values(&r, "n"),
        vec![Value::Int(0), Value::Int(3), Value::Int(6), Value::Int(9)]
    );
}

#[test]
fn generate_series_descending() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(5, 1, -1) AS t(n)", &[])
        .unwrap();
    assert_eq!(
        values(&r, "n"),
        vec![
            Value::Int(5),
            Value::Int(4),
            Value::Int(3),
            Value::Int(2),
            Value::Int(1)
        ]
    );
}

#[test]
fn generate_series_single_value() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(1, 1) AS t(n)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0]["n"], Value::Int(1));
}

#[test]
fn generate_series_empty_range() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(5, 1) AS t(n)", &[])
        .unwrap();
    assert!(r.rows.is_empty());
}

#[test]
fn generate_series_rejects_fractional_and_non_finite_float_bounds() {
    let eng = Engine::new();

    for sql in [
        "SELECT * FROM generate_series(1.5, 3.0)",
        "SELECT * FROM generate_series('NaN'::REAL, 3.0)",
        "SELECT * FROM generate_series(1.0, 'Infinity'::REAL)",
    ] {
        eng.sql(sql, &[])
            .expect_err("float-to-integer series coercion must not truncate or saturate");
    }
}

#[test]
fn unnest_basic() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT val FROM unnest(ARRAY[10, 20, 30]) AS t(val)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 3);
    assert_eq!(
        values(&r, "val"),
        vec![Value::Int(10), Value::Int(20), Value::Int(30)]
    );
}

#[test]
fn unnest_text_array() {
    let eng = Engine::new();
    let r = eng
        .sql(
            "SELECT val FROM unnest(ARRAY['a', 'b', 'c']) AS t(val)",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 3);
    assert_eq!(
        values(&r, "val"),
        vec![
            Value::Str("a".into()),
            Value::Str("b".into()),
            Value::Str("c".into())
        ]
    );
}

#[test]
fn multi_array_unnest_zips_to_the_longest_input_and_null_pads() {
    let eng = Engine::new();
    let aliased = eng
        .sql(
            "SELECT a, b
             FROM unnest(ARRAY[1, 2], ARRAY['foo', 'bar', 'baz']) AS u(a, b)",
            &[],
        )
        .unwrap();
    assert_eq!(aliased.columns, ["a", "b"]);
    assert_eq!(aliased.rows.len(), 3);
    assert_eq!(aliased.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(aliased.value_at(1, 0), Some(&Value::Int(2)));
    assert_eq!(aliased.value_at(2, 0), Some(&Value::Null));
    assert_eq!(aliased.value_at(2, 1), Some(&Value::Str("baz".into())));

    let default_names = eng
        .sql(
            "SELECT * FROM unnest(ARRAY[1, 2], ARRAY['foo', 'bar', 'baz'])",
            &[],
        )
        .unwrap();
    assert_eq!(default_names.columns, ["unnest", "unnest"]);
    assert_eq!(default_names.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(
        default_names.value_at(0, 1),
        Some(&Value::Str("foo".into()))
    );
    assert_eq!(default_names.value_at(2, 0), Some(&Value::Null));
    assert_eq!(
        default_names.value_at(2, 1),
        Some(&Value::Str("baz".into()))
    );

    let partially_aliased = eng
        .sql("SELECT * FROM unnest(ARRAY[1], ARRAY['foo']) AS u(a)", &[])
        .unwrap();
    assert_eq!(partially_aliased.columns, ["a", "unnest"]);
}

#[test]
fn multi_array_unnest_is_rejected_outside_from() {
    let eng = Engine::new();
    let error = eng
        .sql("SELECT unnest(ARRAY[1], ARRAY[2])", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
}

#[test]
fn rows_from_zips_independent_members_and_appends_group_ordinality() {
    let eng = Engine::new();
    let result = eng
        .sql(
            "SELECT number, label, sequence
             FROM ROWS FROM (
                 pg_catalog.generate_series(1, 2),
                 pg_catalog.unnest(ARRAY['a', 'b', 'c'])
             ) WITH ORDINALITY AS rows(number, label, sequence)",
            &[],
        )
        .unwrap();
    assert_eq!(result.columns, ["number", "label", "sequence"]);
    assert_eq!(
        result.column_types,
        [
            Some(ColumnType::Integer),
            Some(ColumnType::Text),
            Some(ColumnType::BigInteger),
        ]
    );
    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(result.value_at(1, 0), Some(&Value::Int(2)));
    assert_eq!(result.value_at(2, 0), Some(&Value::Null));
    assert_eq!(result.value_at(2, 1), Some(&Value::Str("c".into())));
    assert_eq!(result.value_at(2, 2), Some(&Value::Int(3)));

    let empty_member = eng
        .sql(
            "SELECT number, label
             FROM ROWS FROM (
                 pg_catalog.unnest(ARRAY[]::INTEGER[]),
                 pg_catalog.unnest(ARRAY['only'])
             ) AS rows(number, label)",
            &[],
        )
        .unwrap();
    assert_eq!(empty_member.rows.len(), 1);
    assert_eq!(empty_member.value_at(0, 0), Some(&Value::Null));
    assert_eq!(
        empty_member.value_at(0, 1),
        Some(&Value::Str("only".into()))
    );

    let too_many_aliases = eng
        .sql(
            "SELECT * FROM ROWS FROM (pg_catalog.unnest(ARRAY[1])) AS rows(a, b)",
            &[],
        )
        .unwrap_err();
    assert_eq!(too_many_aliases.sqlstate(), Some("42P10"));
}

#[test]
fn rows_from_resolves_each_unary_member_but_multiarg_unnest_bypasses_users() {
    let eng = Engine::new();
    for sql in [
        "CREATE SCHEMA rows_api",
        "CREATE FUNCTION rows_api.unnest(input_values INTEGER[]) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''user-unary'''",
        "CREATE FUNCTION rows_api.unnest(left_values INTEGER[], right_values INTEGER[]) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''user-binary'''",
        "SET search_path = pg_catalog, rows_api, public",
    ] {
        eng.sql(sql, &[]).unwrap();
    }

    let single = eng
        .sql("SELECT chosen FROM unnest(ARRAY[1]::INTEGER[])", &[])
        .unwrap();
    assert_eq!(single.rows[0]["chosen"], Value::Str("user-unary".into()));

    let explicit = eng
        .sql(
            "SELECT chosen, builtin_value
             FROM ROWS FROM (
                 unnest(ARRAY[1]::INTEGER[]),
                 pg_catalog.unnest(ARRAY['a', 'b'])
             ) AS rows(chosen, builtin_value)",
            &[],
        )
        .unwrap();
    assert_eq!(explicit.rows.len(), 2);
    assert_eq!(
        explicit.value_at(0, 0),
        Some(&Value::Str("user-unary".into()))
    );
    assert_eq!(explicit.value_at(0, 1), Some(&Value::Str("a".into())));
    assert_eq!(explicit.value_at(1, 0), Some(&Value::Null));
    assert_eq!(explicit.value_at(1, 1), Some(&Value::Str("b".into())));

    let special = eng
        .sql(
            "SELECT left_value, right_value
             FROM ROWS FROM (
                 unnest(ARRAY[1, 3]::INTEGER[], ARRAY[2]::INTEGER[])
             ) AS rows(left_value, right_value)",
            &[],
        )
        .unwrap();
    assert_eq!(special.rows.len(), 2);
    assert_eq!(special.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(special.value_at(0, 1), Some(&Value::Int(2)));
    assert_eq!(special.value_at(1, 0), Some(&Value::Int(3)));
    assert_eq!(special.value_at(1, 1), Some(&Value::Null));

    let qualified = eng
        .sql(
            "SELECT chosen
             FROM rows_api.unnest(ARRAY[1]::INTEGER[], ARRAY[2]::INTEGER[])",
            &[],
        )
        .unwrap();
    assert_eq!(
        qualified.rows[0]["chosen"],
        Value::Str("user-binary".into())
    );
}

#[test]
fn column_definition_lists_distinguish_named_and_anonymous_records() {
    let eng = Engine::new();
    for sql in [
        "CREATE SCHEMA named_rows",
        "CREATE FUNCTION named_rows.unnest(left_values INTEGER[], right_values INTEGER[]) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''user-table'''",
        "CREATE FUNCTION named_rows.two_columns() RETURNS TABLE(first_value TEXT, second_value INTEGER) LANGUAGE SQL AS 'SELECT ''first'', 2'",
        "SET search_path = pg_catalog, named_rows, public",
    ] {
        eng.sql(sql, &[]).unwrap();
    }

    assert_sql_error(
        &eng,
        "SELECT renamed
         FROM ROWS FROM (
             unnest(ARRAY[1]::INTEGER[], ARRAY[2]::INTEGER[]) AS (renamed TEXT)
         ) AS rows",
        "42601",
        "a column definition list is only allowed for functions returning \"record\"",
    );
    assert_sql_error(
        &eng,
        "SELECT * FROM two_columns() AS (renamed_first TEXT, renamed_second INTEGER)",
        "42601",
        "a column definition list is redundant for a function with OUT parameters",
    );
}

#[test]
fn anonymous_record_table_sources_shape_sql_plpgsql_and_setof_rows() {
    let eng = Engine::new();
    for sql in [
        "CREATE SCHEMA anonymous_rows",
        "CREATE FUNCTION anonymous_rows.unnest(left_values INTEGER[], right_values INTEGER[]) RETURNS record LANGUAGE SQL AS 'SELECT ''user-record''::TEXT, 99::INTEGER'",
        "CREATE FUNCTION anonymous_rows.sql_record_set() RETURNS SETOF record LANGUAGE SQL AS $$ SELECT 'first-record'::TEXT, 1::INTEGER UNION ALL SELECT 'second-record'::TEXT, 2::INTEGER $$",
        "CREATE FUNCTION anonymous_rows.plpgsql_record() RETURNS record LANGUAGE plpgsql AS $$ BEGIN RETURN ROW('plpgsql-record'::TEXT, 100::INTEGER); END $$",
        "SET search_path = pg_catalog, anonymous_rows, public",
    ] {
        eng.sql(sql, &[]).unwrap();
    }

    let sql_record = eng
        .sql(
            "SELECT chosen, marker
             FROM ROWS FROM (
                 unnest(ARRAY[1]::INTEGER[], ARRAY[2]::INTEGER[])
                     AS (chosen TEXT, marker INTEGER)
             ) AS rows",
            &[],
        )
        .unwrap();
    assert_eq!(sql_record.columns, ["chosen", "marker"]);
    assert_eq!(
        sql_record.column_types,
        [Some(ColumnType::Text), Some(ColumnType::Integer)]
    );
    assert_eq!(
        sql_record.value_at(0, 0),
        Some(&Value::Str("user-record".into()))
    );
    assert_eq!(sql_record.value_at(0, 1), Some(&Value::Int(99)));

    let plpgsql_record = eng
        .sql(
            "SELECT chosen, marker
             FROM ROWS FROM (
                 plpgsql_record() AS (chosen TEXT, marker INTEGER)
             ) AS rows",
            &[],
        )
        .unwrap();
    assert_eq!(
        plpgsql_record.value_at(0, 0),
        Some(&Value::Str("plpgsql-record".into()))
    );
    assert_eq!(plpgsql_record.value_at(0, 1), Some(&Value::Int(100)));

    let record_set = eng
        .sql(
            "SELECT chosen, marker
             FROM ROWS FROM (
                 sql_record_set() AS (chosen TEXT, marker INTEGER)
             ) AS rows
             ORDER BY marker",
            &[],
        )
        .unwrap();
    assert_eq!(record_set.rows.len(), 2);
    assert_eq!(
        record_set.value_at(0, 0),
        Some(&Value::Str("first-record".into()))
    );
    assert_eq!(record_set.value_at(1, 1), Some(&Value::Int(2)));

    for sql in [
        "SELECT * FROM anonymous_rows.unnest(ARRAY[1]::INTEGER[], ARRAY[2]::INTEGER[])",
        "SELECT * FROM ROWS FROM (anonymous_rows.unnest(ARRAY[1]::INTEGER[], ARRAY[2]::INTEGER[])) AS rows",
    ] {
        assert_sql_error(
            &eng,
            sql,
            "42601",
            "a column definition list is required for functions returning \"record\"",
        );
    }
}

#[test]
fn anonymous_record_column_definitions_enforce_assignment_compatibility() {
    let eng = Engine::new();
    for sql in [
        "CREATE FUNCTION assignment_record() RETURNS record LANGUAGE SQL AS $$ SELECT 7::INTEGER, 'wide'::TEXT $$",
        "CREATE FUNCTION parseable_text_record() RETURNS record LANGUAGE SQL AS $$ SELECT '42'::TEXT $$",
        "CREATE FUNCTION long_text_record() RETURNS record LANGUAGE SQL AS $$ SELECT 'toolong'::TEXT $$",
        "CREATE FUNCTION plpgsql_text_record() RETURNS record LANGUAGE plpgsql AS $$ BEGIN RETURN ROW('42'::TEXT); END $$",
    ] {
        eng.sql(sql, &[]).unwrap();
    }
    let assignment_compatible = eng
        .sql(
            "SELECT widened, label
             FROM assignment_record() AS (widened BIGINT, label VARCHAR(8))",
            &[],
        )
        .unwrap();
    assert_eq!(
        assignment_compatible.column_types,
        [
            Some(ColumnType::BigInteger),
            Some(ColumnType::Varchar(Some(8))),
        ]
    );
    assert_eq!(assignment_compatible.value_at(0, 0), Some(&Value::Int(7)));
    assert_eq!(
        assignment_compatible.value_at(0, 1),
        Some(&Value::Str("wide".into()))
    );

    for sql in [
        "SELECT * FROM parseable_text_record() AS (parsed INTEGER)",
        "SELECT * FROM plpgsql_text_record() AS (parsed INTEGER)",
        "SELECT * FROM assignment_record() AS (only_column BIGINT)",
    ] {
        assert_sql_error(
            &eng,
            sql,
            "42P13",
            "return type mismatch in function declared to return record",
        );
    }

    assert_sql_error(
        &eng,
        "SELECT * FROM long_text_record() AS (short_value VARCHAR(3))",
        "22001",
        "value too long for type character varying(3)",
    );
}

#[test]
fn json_each_out_columns_reject_redundant_definition_lists() {
    let eng = Engine::new();
    for (function, argument, value_type) in [
        ("json_each", "'{\"a\":1}'::JSON", "JSON"),
        ("jsonb_each", "'{\"a\":1}'::JSONB", "JSONB"),
        ("json_each_text", "'{\"a\":1}'::JSON", "TEXT"),
        ("jsonb_each_text", "'{\"a\":1}'::JSONB", "TEXT"),
    ] {
        for sql in [
            format!(
                "SELECT * FROM pg_catalog.{function}({argument}) AS (k TEXT, v {value_type})"
            ),
            format!(
                "SELECT * FROM ROWS FROM (pg_catalog.{function}({argument}) AS (k TEXT, v {value_type})) AS rows"
            ),
        ] {
            let error = eng.sql(&sql, &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some("42601"), "{sql}");
            assert_eq!(
                error.to_string(),
                "a column definition list is redundant for a function with OUT parameters",
                "{sql}"
            );
        }
    }

    for sql in [
        "CREATE SCHEMA json_shadow",
        "CREATE FUNCTION json_shadow.json_each(input_value JSON) RETURNS record LANGUAGE SQL AS $$ SELECT 'shadow'::TEXT, 7::INTEGER $$",
        "SET search_path = json_shadow, pg_catalog, public",
    ] {
        eng.sql(sql, &[]).unwrap();
    }
    let shadow = eng
        .sql(
            "SELECT source_name, marker
             FROM json_each('{}'::JSON) AS (source_name TEXT, marker INTEGER)",
            &[],
        )
        .unwrap();
    assert_eq!(shadow.value_at(0, 0), Some(&Value::Str("shadow".into())));
    assert_eq!(shadow.value_at(0, 1), Some(&Value::Int(7)));
}

#[test]
fn rows_from_is_implicitly_lateral_and_left_lateral_null_extends_empty_groups() {
    let eng = Engine::new();
    for sql in [
        "CREATE TABLE rows_from_input (id INTEGER PRIMARY KEY, values INTEGER[])",
        "INSERT INTO rows_from_input VALUES (1, ARRAY[10, 20]), (2, ARRAY[]::INTEGER[])",
    ] {
        eng.sql(sql, &[]).unwrap();
    }

    let correlated = eng
        .sql(
            "SELECT input.id, member.value, member.sequence
             FROM rows_from_input AS input
             CROSS JOIN ROWS FROM (pg_catalog.unnest(input.values))
                        WITH ORDINALITY AS member(value, sequence)
             ORDER BY input.id, member.sequence",
            &[],
        )
        .unwrap();
    assert_eq!(correlated.rows.len(), 2);
    assert_eq!(correlated.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(correlated.value_at(0, 1), Some(&Value::Int(10)));
    assert_eq!(correlated.value_at(1, 1), Some(&Value::Int(20)));
    assert_eq!(correlated.value_at(1, 2), Some(&Value::Int(2)));

    let null_extended = eng
        .sql(
            "SELECT input.id, member.value
             FROM rows_from_input AS input
             LEFT JOIN LATERAL ROWS FROM (pg_catalog.unnest(input.values))
                  AS member(value) ON TRUE
             ORDER BY input.id, member.value",
            &[],
        )
        .unwrap();
    assert_eq!(null_extended.rows.len(), 3);
    assert_eq!(null_extended.value_at(2, 0), Some(&Value::Int(2)));
    assert_eq!(null_extended.value_at(2, 1), Some(&Value::Null));
}

#[test]
fn lateral_rows_from_spills_correlated_function_groups_under_tiny_work_mem() {
    let eng = Engine::new();
    eng.sql("SET work_mem TO '1B'", &[]).unwrap();

    let result = eng
        .sql(
            "SELECT count(*) AS total_rows,
                    count(member.short_value) AS complete_short_rows
             FROM (VALUES (4096), (8192)) AS input(upper_bound)
             CROSS JOIN LATERAL ROWS FROM (
                 pg_catalog.generate_series(1, input.upper_bound),
                 pg_catalog.generate_series(1, input.upper_bound - 1)
             ) AS member(long_value, short_value)",
            &[],
        )
        .unwrap();

    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(12_288)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(12_286)));
}

#[test]
fn rows_from_member_bindings_survive_view_reopen_and_search_path_changes() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("rows-from-view.db");
    {
        let eng = Engine::open(&database).unwrap();
        for sql in [
            "CREATE SCHEMA rows_first",
            "CREATE SCHEMA rows_second",
            "CREATE FUNCTION rows_first.group_pick(value BIGINT) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''first'''",
            "CREATE FUNCTION rows_second.group_pick(value BIGINT) RETURNS TABLE(chosen TEXT) LANGUAGE SQL AS 'SELECT ''second'''",
            "SET search_path = rows_first, rows_second, public",
            "CREATE VIEW rows_first.bound_group AS SELECT chosen, number FROM ROWS FROM (group_pick(7::BIGINT), pg_catalog.unnest(ARRAY[1, 2])) AS rows(chosen, number)",
        ] {
            eng.sql(sql, &[]).unwrap();
        }
    }

    let reopened = Engine::open(&database).unwrap();
    reopened
        .sql("SET search_path = rows_second, rows_first, public", &[])
        .unwrap();
    let result = reopened
        .sql(
            "SELECT chosen, number FROM rows_first.bound_group ORDER BY number",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.value_at(0, 0), Some(&Value::Str("first".into())));
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(1)));
    assert_eq!(result.value_at(1, 0), Some(&Value::Null));
    assert_eq!(result.value_at(1, 1), Some(&Value::Int(2)));
}

#[test]
fn rows_from_exact_overload_bindings_reach_update_delete_and_merge() {
    let eng = Engine::new();
    for sql in [
        "CREATE FUNCTION rows_dml_pick(value INTEGER) RETURNS TABLE(int4_value TEXT) LANGUAGE SQL AS 'SELECT ''int4'''",
        "CREATE FUNCTION rows_dml_pick(value BIGINT) RETURNS TABLE(int8_value TEXT) LANGUAGE SQL AS 'SELECT ''int8'''",
        "CREATE TABLE rows_dml_target (id INTEGER PRIMARY KEY, chosen TEXT)",
        "INSERT INTO rows_dml_target VALUES (1, 'old'), (2, 'old'), (3, 'old')",
    ] {
        eng.sql(sql, &[]).unwrap();
    }

    let updated = eng
        .sql(
            "UPDATE rows_dml_target AS target
             SET chosen = source.int8_value
             FROM ROWS FROM (rows_dml_pick((SELECT 7::BIGINT))) AS source(int8_value)
             WHERE target.id = 1
             RETURNING target.chosen AS v",
            &[],
        )
        .unwrap();
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(updated.rows[0]["v"], Value::Str("int8".into()));

    let deleted = eng
        .sql(
            "DELETE FROM rows_dml_target AS target
             USING ROWS FROM (rows_dml_pick((SELECT 7::BIGINT))) AS source(int8_value)
             WHERE target.id = 2 AND source.int8_value = 'int8'
             RETURNING source.int8_value AS v",
            &[],
        )
        .unwrap();
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(deleted.rows[0]["v"], Value::Str("int8".into()));

    let merged = eng
        .sql(
            "MERGE INTO rows_dml_target AS target
             USING ROWS FROM (rows_dml_pick((SELECT 7::BIGINT))) AS source(int8_value)
             ON target.id = 3
             WHEN MATCHED THEN UPDATE SET chosen = source.int8_value
             RETURNING target.chosen AS v",
            &[],
        )
        .unwrap();
    assert_eq!(merged.affected_rows, 1);
    assert_eq!(merged.rows[0]["v"], Value::Str("int8".into()));
}

#[test]
fn values_in_from_with_aliased_columns() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT id FROM (VALUES (1), (2), (3)) AS t(id)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 3);
}

#[test]
fn standalone_values_returns_rows() {
    let eng = Engine::new();
    let r = eng.sql("VALUES (1, 'a'), (2, 'b')", &[]).unwrap();
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0].get("column1"), Some(&Value::Int(1)));
    assert_eq!(r.rows[1].get("column2"), Some(&Value::Str("b".into())));
}

#[test]
fn values_coerce_unknown_literals_to_the_postgresql_common_type() {
    let eng = Engine::new();

    let dates = eng
        .sql("VALUES (DATE '2020-01-01'), ('2020-01-02')", &[])
        .unwrap();
    assert_eq!(dates.column_types, [Some(ColumnType::Date)]);
    assert!(dates
        .rows
        .iter()
        .all(|row| matches!(row["column1"], Value::Temporal(_))));

    let dates_from = eng
        .sql(
            "SELECT value FROM (VALUES (DATE '2020-01-01'), ('2020-01-02')) AS source(value)",
            &[],
        )
        .unwrap();
    assert_eq!(dates_from.column_types, [Some(ColumnType::Date)]);
    assert!(dates_from
        .rows
        .iter()
        .all(|row| matches!(row["value"], Value::Temporal(_))));

    let integers = eng.sql("VALUES (1), ('2')", &[]).unwrap();
    assert_eq!(integers.column_types, [Some(ColumnType::Integer)]);
    assert_eq!(integers.rows[1]["column1"], Value::Int(2));

    let booleans = eng.sql("VALUES (TRUE), ('false')", &[]).unwrap();
    assert_eq!(booleans.column_types, [Some(ColumnType::Boolean)]);
    assert_eq!(booleans.rows[1]["column1"], Value::Bool(false));
}
