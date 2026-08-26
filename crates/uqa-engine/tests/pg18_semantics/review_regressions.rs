//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ColumnType;

fn assert_sqlstate(engine: &Engine, sql: &str, expected: &str) {
    let error = engine.sql(sql, &[]).expect_err(sql);
    assert_eq!(
        error.sqlstate(),
        Some(expected),
        "unexpected error: {error}"
    );
}

#[test]
fn empty_array_growth_and_array_ordering_match_postgresql_18() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, "SELECT array_append('{}'::int[], 1)"),
        array(vec![Value::Int(1)])
    );
    assert_eq!(
        scalar(&eng, "SELECT array_prepend(1, '{}'::int[])"),
        array(vec![Value::Int(1)])
    );
    for sql in [
        "SELECT ARRAY[1] < ARRAY[2]",
        "SELECT ARRAY[1,2] < ARRAY[[1,2]]",
        "SELECT ARRAY[2,0] > ARRAY[[1,9]]",
        "SELECT ARRAY[1] < '[2:2]={1}'::int[]",
    ] {
        assert_eq!(scalar(&eng, sql), Value::Bool(true), "{sql}");
    }
}

#[test]
fn compiler_owned_dispatch_does_not_reserve_user_function_names() {
    let eng = engine();
    for name in [
        "__named_arg",
        "__variadic_arg",
        "__subscript",
        "__array_subscripts",
        "__is_distinct",
        "__between_symmetric",
        "__any_op",
        "__to_hex_int4",
        "__random_int4_range",
        "__array_sort_json",
        "__range_lower_int4range",
    ] {
        let create = format!(
            "CREATE FUNCTION {name}() RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT ''{name}'''"
        );
        eng.sql(&create, &[])
            .unwrap_or_else(|error| panic!("{create}: {error}"));
        assert_eq!(text(&eng, &format!("SELECT {name}()")), name);
    }
    assert_eq!(scalar(&eng, "SELECT (ARRAY[10, 20])[2]"), Value::Int(20));
    assert_eq!(
        scalar(&eng, "SELECT 1 IS DISTINCT FROM NULL"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT 1 = ANY(ARRAY[0, 1])"),
        Value::Bool(true)
    );
}

fn assert_ordinary_metadata_named_columns(eng: &Engine) {
    eng.sql(
        "CREATE TABLE metadata_named_columns (
            id INTEGER PRIMARY KEY,
            _score INTEGER,
            _doc_id TEXT,
            _merge_action TEXT
        )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO metadata_named_columns VALUES (1, 42, 'document-user', 'user-value')",
        &[],
    )
    .unwrap();

    let result = eng
        .sql("SELECT * FROM metadata_named_columns", &[])
        .unwrap();
    assert_eq!(
        result.columns,
        vec!["id", "_score", "_doc_id", "_merge_action"]
    );
    assert_eq!(result.rows[0]["_score"], Value::Int(42));
    assert_eq!(
        result.rows[0]["_doc_id"],
        Value::Str("document-user".into())
    );
    assert_eq!(
        result.rows[0]["_merge_action"],
        Value::Str("user-value".into())
    );

    let derived = eng
        .sql(
            "SELECT * FROM (
                SELECT _score, _doc_id, _merge_action FROM metadata_named_columns
             ) AS projected",
            &[],
        )
        .unwrap();
    assert_eq!(derived.columns, vec!["_score", "_doc_id", "_merge_action"]);
    assert_eq!(derived.rows[0]["_score"], Value::Int(42));

    let metadata = eng
        .sql(
            "SELECT *, _meta.score AS system_score, _meta.doc_id AS system_doc_id
             FROM metadata_named_columns",
            &[],
        )
        .unwrap();
    assert_eq!(metadata.rows[0]["_score"], Value::Int(42));
    assert_eq!(
        metadata.rows[0]["_doc_id"],
        Value::Str("document-user".into())
    );
    assert_eq!(metadata.rows[0]["system_score"], Value::Float(0.0));
    assert_eq!(metadata.rows[0]["system_doc_id"], Value::Int(1));
    assert_eq!(
        &metadata.column_types[metadata.column_types.len() - 2..],
        [
            Some(ColumnType::DoublePrecision),
            Some(ColumnType::BigInteger)
        ]
    );

    let joined = eng
        .sql(
            "SELECT _meta.doc_id AS system_doc_id, _meta.score AS system_score
             FROM metadata_named_columns AS source
             CROSS JOIN (VALUES (1)) AS marker(value)",
            &[],
        )
        .unwrap();
    assert_eq!(joined.rows[0]["system_doc_id"], Value::Int(1));
    assert_eq!(joined.rows[0]["system_score"], Value::Float(0.0));

    let updated = eng
        .sql(
            "UPDATE metadata_named_columns
             SET _doc_id = 'document-after'
             RETURNING _doc_id",
            &[],
        )
        .unwrap();
    assert_eq!(
        updated.rows[0]["_doc_id"],
        Value::Str("document-after".into())
    );
}

fn assert_ranked_metadata_named_columns(eng: &Engine) {
    eng.sql(
        "CREATE TABLE ranked_metadata_name (
            id INTEGER PRIMARY KEY,
            body TEXT,
            _score INTEGER,
            _doc_id TEXT
        )",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX ranked_metadata_name_body_gin
         ON ranked_metadata_name USING gin (body)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO ranked_metadata_name VALUES
            (1, 'alpha alpha alpha', 1, 'ranked-one'),
            (2, 'alpha', 100, 'ranked-two')",
        &[],
    )
    .unwrap();
    let ranked = eng
        .sql(
            "SELECT _score, _doc_id,
                    _meta.score AS system_score,
                    _meta.doc_id AS system_doc_id,
                    score_bm25(body, 'alpha') AS retrieval_score
             FROM ranked_metadata_name
             WHERE text_match(body, 'alpha') AND id = 1",
            &[],
        )
        .unwrap();
    assert_eq!(ranked.rows[0]["_score"], Value::Int(1));
    assert_eq!(ranked.rows[0]["_doc_id"], Value::Str("ranked-one".into()));
    assert_eq!(ranked.rows[0]["system_doc_id"], Value::Int(1));
    assert_eq!(
        ranked.rows[0]["system_score"],
        ranked.rows[0]["retrieval_score"]
    );
    assert!(matches!(ranked.rows[0]["system_score"], Value::Float(_)));

    let system_ordered = eng
        .sql(
            "SELECT id, _meta.score AS system_score
             FROM ranked_metadata_name
             WHERE text_match(body, 'alpha')
             ORDER BY _meta.score DESC, id",
            &[],
        )
        .unwrap();
    let first_score = system_ordered.rows[0]["system_score"].clone();
    let second_score = system_ordered.rows[1]["system_score"].clone();
    assert!(
        matches!((first_score, second_score), (Value::Float(first), Value::Float(second)) if first >= second)
    );

    let aliased = eng
        .sql(
            "SELECT hit._score, hit._doc_id,
                    _meta.score AS system_score,
                    _meta.doc_id AS system_doc_id
             FROM ranked_metadata_name AS hit
             WHERE text_match(hit.body, 'alpha') AND hit.id = 1",
            &[],
        )
        .unwrap();
    assert_eq!(aliased.rows[0]["_score"], Value::Int(1));
    assert_eq!(aliased.rows[0]["_doc_id"], Value::Str("ranked-one".into()));
    assert_eq!(aliased.rows[0]["system_doc_id"], Value::Int(1));
    assert!(matches!(aliased.rows[0]["system_score"], Value::Float(_)));

    let ordered = eng
        .sql(
            "SELECT id, _score FROM ranked_metadata_name
             WHERE text_match(body, 'alpha')
             ORDER BY _score DESC LIMIT 1",
            &[],
        )
        .unwrap();
    assert_eq!(ordered.rows[0]["id"], Value::Int(2));
    assert_eq!(ordered.rows[0]["_score"], Value::Int(100));

    let mixed = eng
        .sql(
            "SELECT id, _doc_id FROM ranked_metadata_name
             WHERE text_match(body, 'alpha') OR _doc_id = 'absent'
             ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(mixed.rows[0]["_doc_id"], Value::Str("ranked-one".into()));
    assert_eq!(mixed.rows[1]["_doc_id"], Value::Str("ranked-two".into()));
}

#[test]
fn user_columns_that_resemble_engine_metadata_remain_visible() {
    let eng = engine();
    assert_ordinary_metadata_named_columns(&eng);
    assert_ranked_metadata_named_columns(&eng);
}

#[test]
fn a_real_meta_relation_alias_keeps_normal_sql_name_resolution() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE meta_alias_source (id INTEGER, score INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO meta_alias_source VALUES (1, 17)", &[])
        .unwrap();

    let aliases = eng
        .sql(
            "SELECT _doc_id AS legacy_doc_id,
                    _meta.doc_id AS namespaced_doc_id,
                    _score AS legacy_score,
                    _meta.score AS namespaced_score
             FROM meta_alias_source",
            &[],
        )
        .unwrap();
    assert_eq!(
        aliases.rows[0]["legacy_doc_id"],
        aliases.rows[0]["namespaced_doc_id"]
    );
    assert_eq!(
        aliases.rows[0]["legacy_score"],
        aliases.rows[0]["namespaced_score"]
    );

    assert_eq!(
        scalar(&eng, "SELECT _meta.score FROM meta_alias_source AS _meta"),
        Value::Int(17)
    );
    assert_sqlstate(
        &eng,
        "SELECT _meta.doc_id
         FROM meta_alias_source AS left_source
         CROSS JOIN meta_alias_source AS right_source",
        "42P01",
    );
    assert_sqlstate(&eng, "SELECT _meta.missing FROM meta_alias_source", "42703");
}

#[test]
fn array_element_cast_uses_the_declared_source_width() {
    let eng = engine();
    assert_eq!(
        text(
            &eng,
            "SELECT encode((ARRAY[1::smallint]::bytea[])[1], 'hex')"
        ),
        "0001"
    );
}

#[test]
fn common_type_selection_keeps_unknown_literals_until_context_resolution() {
    let eng = engine();
    assert_eq!(
        text(&eng, "SELECT pg_typeof(ARRAY['x', 'y'::varchar])"),
        "character varying[]"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT pg_typeof(CASE WHEN true THEN 'x' ELSE 'y'::varchar END)"
        ),
        "character varying"
    );
    assert_eq!(
        text(&eng, "SELECT pg_typeof(COALESCE('x', 'y'::varchar))"),
        "character varying"
    );
}

#[test]
fn common_type_selection_coerces_runtime_values_before_aggregation() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE mixed_common_type (g INTEGER, floating DOUBLE PRECISION, exact NUMERIC)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO mixed_common_type VALUES (1, NULL, 1.25), (1, 2.5, 9.75), (2, NULL, 4.5)",
        &[],
    )
    .unwrap();
    eng.sql("SET work_mem TO '1B'", &[]).unwrap();

    let rows = eng
        .sql(
            "SELECT g,
                    pg_typeof(SUM(COALESCE(floating, exact))) AS sum_type,
                    SUM(COALESCE(floating, exact)) AS coalesced,
                    SUM(CASE WHEN floating IS NULL THEN exact ELSE floating END) AS conditional
             FROM mixed_common_type
             GROUP BY g
             ORDER BY g",
            &[],
        )
        .unwrap()
        .rows;

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["sum_type"], Value::Str("double precision".into()));
    assert_eq!(rows[0]["coalesced"], Value::Float(3.75));
    assert_eq!(rows[0]["conditional"], Value::Float(3.75));
    assert_eq!(rows[1]["coalesced"], Value::Float(4.5));
    assert_eq!(rows[1]["conditional"], Value::Float(4.5));
}

#[test]
fn numeric_parser_and_extreme_scale_arithmetic_match_postgresql_18() {
    let eng = engine();
    assert_sqlstate(&eng, "SELECT '-+1'::numeric", "22P02");
    assert_sqlstate(&eng, "SELECT '+NaN'::numeric", "22P02");
    assert_eq!(
        scalar(&eng, "SELECT 0e200000::numeric = 0"),
        Value::Bool(true)
    );
    let Value::Decimal(product) = scalar(&eng, "SELECT 1e-9000::numeric * 1e-9000::numeric") else {
        panic!("expected numeric product");
    };
    assert!(product.is_zero());
    assert_eq!(product.to_sql_string().len(), 16_385);
}

#[test]
fn invalid_date_fields_and_formats_report_postgresql_sqlstates() {
    let eng = engine();
    assert_sqlstate(&eng, "SELECT make_date(2023, 2, 29)", "22008");
    assert_sqlstate(&eng, "SELECT date '2024-02-30'", "22008");
    assert_sqlstate(&eng, "SELECT date 'not-a-date'", "22007");
}

#[test]
fn escaped_array_literal_whitespace_and_null_text_are_significant() {
    let eng = engine();
    assert_eq!(
        scalar(&eng, r"SELECT ('{\ a}'::text[])[1]"),
        Value::Str(" a".into())
    );
    assert_eq!(
        scalar(&eng, r"SELECT ('{N\ULL}'::text[])[1]"),
        Value::Str("NULL".into())
    );
}

#[test]
fn join_using_and_returning_alias_collisions_report_duplicate_relation() {
    let eng = engine();
    assert_sqlstate(
        &eng,
        "SELECT * FROM (VALUES (1)) AS l(id) FULL JOIN (VALUES (1)) AS r(id) USING (id) AS l",
        "42712",
    );
    eng.sql("CREATE TABLE review_returning (id INTEGER)", &[])
        .unwrap();
    assert_sqlstate(
        &eng,
        "INSERT INTO review_returning VALUES (1) RETURNING WITH (OLD AS review_returning, NEW AS after) after.id",
        "42712",
    );
    assert_sqlstate(
        &eng,
        "INSERT INTO review_returning VALUES (2) RETURNING WITH (OLD AS image, NEW AS image) image.id",
        "42712",
    );
    assert_sqlstate(
        &eng,
        "INSERT INTO review_returning VALUES (3) RETURNING WITH (OLD AS first, OLD AS second) first.id",
        "42601",
    );
}

#[test]
fn numeric_to_char_places_sign_tokens_at_their_postgresql_positions() {
    let eng = engine();
    for (sql, expected) in [
        ("SELECT to_char(12::numeric, 'PL999')", "+  12"),
        ("SELECT to_char(-12::numeric, 'PL999')", "  -12"),
        ("SELECT to_char(12::numeric, 'SG999')", "+ 12"),
        ("SELECT to_char(-12::numeric, 'SG999')", "- 12"),
        ("SELECT to_char(12::numeric, '9SG99')", " +12"),
        ("SELECT to_char(-12::numeric, '9MI99')", " -12"),
        ("SELECT to_char(12::numeric, '9S9.9')", "+12.0"),
        ("SELECT to_char(-12::numeric, '99S.9')", "12.0-"),
        ("SELECT to_char(12::numeric, '9MI99SG')", "  12+"),
        ("SELECT to_char(-12::numeric, '9MI99SG')", " -12-"),
        ("SELECT to_char(1::numeric, 'FM9.9MIPL')", "1.+"),
        ("SELECT to_char(-1::numeric, 'FM9.9MIPL')", "1.-"),
        ("SELECT to_char(-12::numeric, '999S,')", " 12-,"),
        ("SELECT to_char(-12::numeric, '999,S')", " 12-,"),
        ("SELECT to_char(-12::numeric, '999PR,')", " <12>,"),
        ("SELECT to_char(12::numeric, '999PR,')", "  12 ,"),
    ] {
        assert_eq!(text(&eng, sql), expected, "{sql}");
    }
    assert_sqlstate(&eng, "SELECT to_char(12::numeric, 'PR999')", "42601");
    assert_sqlstate(&eng, "SELECT to_char(12::numeric, '9S99MI')", "42601");
}

#[test]
fn to_hex_uses_the_declared_integer_overload_at_every_expression_boundary() {
    let eng = engine();
    for (sql, sqlstate) in [
        ("SELECT to_hex('42')", "42725"),
        ("SELECT to_hex(NULL)", "42725"),
        ("SELECT to_hex(1::smallint)", "42725"),
        ("SELECT to_hex('42'::text)", "42883"),
    ] {
        assert_sqlstate(&eng, sql, sqlstate);
    }
    assert_eq!(scalar(&eng, "SELECT to_hex(NULL::integer)"), Value::Null);
    assert_eq!(scalar(&eng, "SELECT to_hex(NULL::bigint)"), Value::Null);
    assert_eq!(
        text(&eng, "SELECT to_hex((-1)::bigint)"),
        "ffffffffffffffff"
    );
    eng.sql(
        "CREATE TABLE to_hex_widths (
            i4 INTEGER,
            i8 BIGINT,
            default_hex TEXT DEFAULT to_hex((-1)::bigint),
            CHECK (to_hex(i8) = 'ffffffffffffffff')
        )",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO to_hex_widths (i4, i8) VALUES (-1, -1)", &[])
        .unwrap();
    assert_sqlstate(
        &eng,
        "INSERT INTO to_hex_widths (i4, i8) VALUES (-1, 0)",
        "23514",
    );
    assert_eq!(
        text(&eng, "SELECT to_hex(i4) FROM to_hex_widths"),
        "ffffffff"
    );
    assert_eq!(
        text(&eng, "SELECT to_hex(i8) FROM to_hex_widths"),
        "ffffffffffffffff"
    );
    assert_eq!(
        text(&eng, "SELECT default_hex FROM to_hex_widths"),
        "ffffffffffffffff"
    );
    eng.sql("CREATE TABLE to_hex_alter (i8 BIGINT)", &[])
        .unwrap();
    eng.sql("INSERT INTO to_hex_alter VALUES (-1)", &[])
        .unwrap();
    eng.sql(
        "ALTER TABLE to_hex_alter ALTER COLUMN i8 TYPE TEXT USING to_hex(i8)",
        &[],
    )
    .unwrap();
    assert_eq!(
        text(&eng, "SELECT i8 FROM to_hex_alter"),
        "ffffffffffffffff"
    );
    assert_sqlstate(
        &eng,
        "CREATE TABLE invalid_default_reference (i8 BIGINT, encoded TEXT DEFAULT to_hex(i8))",
        "0A000",
    );
    assert_sqlstate(
        &eng,
        "CREATE TABLE ambiguous_default (encoded TEXT DEFAULT to_hex('42'))",
        "42725",
    );
    eng.sql("CREATE TABLE invalid_alter_default (i8 BIGINT)", &[])
        .unwrap();
    assert_sqlstate(
        &eng,
        "ALTER TABLE invalid_alter_default ALTER COLUMN i8 SET DEFAULT to_hex(i8)",
        "0A000",
    );
    eng.sql("CREATE TABLE invalid_to_hex_dml (encoded TEXT)", &[])
        .unwrap();
    assert_sqlstate(
        &eng,
        "INSERT INTO invalid_to_hex_dml VALUES (to_hex('42'))",
        "42725",
    );
}

#[test]
fn to_bin_and_to_oct_match_postgresql_integer_widths_and_binding() {
    let eng = engine();
    assert_integer_base_binding_errors(&eng);
    assert_integer_base_results(&eng);
    assert_integer_base_ddl_boundaries(&eng);
}

fn assert_integer_base_binding_errors(eng: &Engine) {
    for function in ["to_bin", "to_oct"] {
        for (argument, sqlstate) in [
            ("'42'", "42725"),
            ("NULL", "42725"),
            ("1::smallint", "42725"),
            ("'42'::text", "42883"),
            ("1::numeric", "42883"),
        ] {
            let sql = format!("SELECT {function}({argument})");
            assert_sqlstate(eng, &sql, sqlstate);
        }
        assert_sqlstate(eng, &format!("SELECT {function}()"), "42883");
        assert_sqlstate(eng, &format!("SELECT {function}(1, 2)"), "42883");
        let named_sql = format!("SELECT {function}(value => 1)");
        let named_error = eng.sql(&named_sql, &[]).expect_err(&named_sql);
        assert_eq!(named_error.sqlstate(), Some("42883"));
        assert!(named_error
            .to_string()
            .contains(&format!("function {function}(")));
        assert!(!named_error.to_string().contains("__to_"));
        assert_sqlstate(
            eng,
            &format!("SELECT {function}((SELECT 1::smallint))"),
            "42725",
        );
        assert_sqlstate(eng, &format!("SELECT {function}((SELECT '42'))"), "42883");
        assert_eq!(
            scalar(eng, &format!("SELECT {function}(NULL::integer)")),
            Value::Null
        );
        assert_eq!(
            scalar(eng, &format!("SELECT {function}(NULL::bigint)")),
            Value::Null
        );
    }
}

fn assert_integer_base_results(eng: &Engine) {
    for (sql, expected) in [
        ("SELECT to_bin(0)", "0"),
        ("SELECT to_bin(42)", "101010"),
        ("SELECT to_bin(-42)", "11111111111111111111111111010110"),
        (
            "SELECT to_bin((-42)::bigint)",
            "1111111111111111111111111111111111111111111111111111111111010110",
        ),
        (
            "SELECT to_bin((-2147483648)::integer)",
            "10000000000000000000000000000000",
        ),
        (
            "SELECT to_bin((-9223372036854775807 - 1)::bigint)",
            "1000000000000000000000000000000000000000000000000000000000000000",
        ),
        (
            "SELECT to_bin((SELECT (-1)::bigint))",
            "1111111111111111111111111111111111111111111111111111111111111111",
        ),
        ("SELECT to_oct(0)", "0"),
        ("SELECT to_oct(42)", "52"),
        ("SELECT to_oct(-42)", "37777777726"),
        ("SELECT to_oct((-42)::bigint)", "1777777777777777777726"),
        ("SELECT to_oct((-2147483648)::integer)", "20000000000"),
        (
            "SELECT to_oct((-9223372036854775807 - 1)::bigint)",
            "1000000000000000000000",
        ),
        (
            "SELECT to_oct((SELECT (-1)::bigint))",
            "1777777777777777777777",
        ),
        ("SELECT pg_catalog.to_bin(42)", "101010"),
        ("SELECT pg_catalog.to_oct(42)", "52"),
    ] {
        assert_eq!(text(eng, sql), expected, "{sql}");
    }
    for sql in [
        "SELECT pg_typeof(to_bin(42))",
        "SELECT pg_typeof(to_oct(42::bigint))",
    ] {
        assert_eq!(text(eng, sql), "text", "{sql}");
    }
}

fn assert_integer_base_ddl_boundaries(eng: &Engine) {
    eng.sql(
        "CREATE TABLE integer_base_widths (
            i4 INTEGER,
            i8 BIGINT,
            bin4 TEXT GENERATED ALWAYS AS (to_bin(i4)) STORED,
            oct8 TEXT GENERATED ALWAYS AS (to_oct(i8)) STORED,
            default_bin TEXT DEFAULT to_bin((-1)::bigint),
            CHECK (to_oct(i4) = '37777777777')
        )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO integer_base_widths (i4, i8) VALUES (-1, -1)",
        &[],
    )
    .unwrap();
    for (column, expected) in [
        ("bin4", "11111111111111111111111111111111"),
        ("oct8", "1777777777777777777777"),
        (
            "default_bin",
            "1111111111111111111111111111111111111111111111111111111111111111",
        ),
    ] {
        assert_eq!(
            text(eng, &format!("SELECT {column} FROM integer_base_widths")),
            expected
        );
    }
    assert_sqlstate(
        eng,
        "INSERT INTO integer_base_widths (i4, i8) VALUES (0, 0)",
        "23514",
    );
    assert_sqlstate(
        eng,
        "CREATE TABLE ambiguous_generated_base (i2 SMALLINT, encoded TEXT GENERATED ALWAYS AS (to_bin(i2)) STORED)",
        "42725",
    );
    eng.sql(
        "CREATE VIEW integer_base_subquery_view AS
         SELECT to_bin((SELECT (-1)::bigint)) AS encoded",
        &[],
    )
    .unwrap();
    assert_eq!(
        text(eng, "SELECT encoded FROM integer_base_subquery_view"),
        "1111111111111111111111111111111111111111111111111111111111111111"
    );
    assert_sqlstate(
        eng,
        "CREATE VIEW invalid_integer_base_subquery_view AS SELECT to_oct((SELECT '42')) AS encoded",
        "42883",
    );
    assert_sqlstate(
        eng,
        "CREATE VIEW invalid_integer_base_named_view AS SELECT to_bin(value => 1) AS encoded",
        "42883",
    );
}
