//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Names in SQL function bodies, resolved as `PostgreSQL` 18's parser resolves them with the hooks of `sql_fn_parser_setup`: a column of any query level, an output column named by ORDER BY, GROUP BY or DISTINCT ON, and a relation used as a whole-row value each take a name before a parameter does; the routine's name qualifies a parameter; and each clause of a data-modifying statement sees only the relations of its own namespace.

#[path = "sql_function_parameters/defaults.rs"]
mod defaults;

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::{SQLError, SQLResult};

fn sql(engine: &Engine, statement: &str) -> SQLResult {
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"))
}

fn value(engine: &Engine, query: &str) -> Value {
    let result = sql(engine, query);
    let column = &result.columns[0];
    result.rows[0][column.as_str()].clone()
}

fn error(engine: &Engine, statement: &str) -> SQLError {
    match engine.sql(statement, &[]) {
        Ok(_) => panic!("{statement} succeeded"),
        Err(error) => error,
    }
}

fn expect(engine: &Engine, statement: &str, sqlstate: &str, message: &str) {
    let error = error(engine, statement);
    assert_eq!(
        (error.sqlstate().unwrap_or_default(), error.to_string()),
        (sqlstate, message.to_string()),
        "{statement}"
    );
}

fn tables() -> Engine {
    let engine = Engine::new();
    for statement in [
        "CREATE TABLE t (id int, v int)",
        "INSERT INTO t VALUES (10, 1), (20, 2)",
        "CREATE TABLE tv (id int PRIMARY KEY, v int)",
        "CREATE TABLE src (id int, w int)",
        "INSERT INTO src VALUES (7, 70)",
    ] {
        sql(&engine, statement);
    }
    engine
}

/// Define each routine and compare its call with what `PostgreSQL` returns.
fn check(engine: &Engine, cases: &[(&str, &str, Value)]) {
    for (definition, call, expected) in cases {
        sql(engine, definition);
        assert_eq!(&value(engine, call), expected, "{definition}");
    }
}

#[test]
fn a_column_of_any_query_level_takes_a_name_before_a_parameter() {
    let engine = tables();
    check(
        &engine,
        &[
            (
                "CREATE FUNCTION s1(id int) RETURNS int LANGUAGE sql AS 'SELECT id FROM t ORDER BY id LIMIT 1'",
                "SELECT s1(99)",
                Value::Int(10),
            ),
            (
                "CREATE FUNCTION s3(id int) RETURNS int LANGUAGE sql AS 'SELECT v FROM t WHERE id = id ORDER BY v LIMIT 1'",
                "SELECT s3(99)",
                Value::Int(1),
            ),
            (
                "CREATE FUNCTION s4(v int) RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM t WHERE id > v'",
                "SELECT s4(100)",
                Value::Int(2),
            ),
            (
                "CREATE FUNCTION sq(id int) RETURNS int LANGUAGE sql AS 'SELECT (SELECT id) FROM t ORDER BY 1 LIMIT 1'",
                "SELECT sq(5)",
                Value::Int(10),
            ),
            (
                "CREATE FUNCTION lat(v int) RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM t, LATERAL (SELECT v AS x) l WHERE l.x = t.v'",
                "SELECT lat(100)",
                Value::Int(2),
            ),
            (
                "CREATE FUNCTION jo(w int) RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM t JOIN src ON t.v < w'",
                "SELECT jo(2)",
                Value::Int(2),
            ),
            (
                "CREATE FUNCTION hv(v int) RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM (SELECT id FROM t GROUP BY id HAVING max(v) > 1) s'",
                "SELECT hv(0)",
                Value::Int(1),
            ),
            // A name that no queried relation has is the parameter, even beside a common table expression.
            (
                "CREATE FUNCTION cte(v int) RETURNS bigint LANGUAGE sql AS 'WITH c AS (SELECT v AS x FROM t) SELECT count(*) FROM c WHERE x < v'",
                "SELECT cte(2)",
                Value::Int(1),
            ),
            (
                "CREATE FUNCTION vf(id int) RETURNS int LANGUAGE sql AS 'SELECT a FROM (VALUES (id)) AS x(a)'",
                "SELECT vf(8)",
                Value::Int(8),
            ),
            (
                "CREATE FUNCTION lim(n int) RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM (SELECT 1 FROM t LIMIT n) s'",
                "SELECT lim(1)",
                Value::Int(1),
            ),
        ],
    );
    // A column takes the name in LIMIT too, where a column of the query's own level is not allowed.
    expect(
        &engine,
        "CREATE FUNCTION lv(v int) RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM (SELECT 1 FROM t LIMIT v) s'",
        "42P10",
        "argument of LIMIT must not contain variables",
    );
}

#[test]
fn the_routine_name_qualifies_a_parameter_that_no_relation_takes() {
    let engine = tables();
    check(
        &engine,
        &[
            (
                "CREATE FUNCTION s2(id int) RETURNS int LANGUAGE sql AS 'SELECT s2.id FROM t LIMIT 1'",
                "SELECT s2(99)",
                Value::Int(99),
            ),
            (
                "CREATE FUNCTION sq2(id int) RETURNS int LANGUAGE sql AS 'SELECT (SELECT sq2.id) FROM t LIMIT 1'",
                "SELECT sq2(5)",
                Value::Int(5),
            ),
            // A relation that takes the routine's name takes the qualified name too, unless it lacks the column.
            (
                "CREATE FUNCTION fa(id int) RETURNS int LANGUAGE sql AS 'SELECT fa.id FROM t AS fa ORDER BY 1 LIMIT 1'",
                "SELECT fa(5)",
                Value::Int(10),
            ),
            (
                "CREATE FUNCTION g(a int) RETURNS int LANGUAGE sql AS 'SELECT g.a FROM t AS g LIMIT 1'",
                "SELECT g(77)",
                Value::Int(77),
            ),
            (
                "CREATE FUNCTION f(f int) RETURNS int LANGUAGE sql AS 'SELECT f'",
                "SELECT f(4)",
                Value::Int(4),
            ),
            // A parameter reference names its output column as written.
            (
                "CREATE FUNCTION lb(n int) RETURNS int LANGUAGE sql AS 'SELECT s.n FROM (SELECT n) s'",
                "SELECT lb(7)",
                Value::Int(7),
            ),
            (
                "CREATE FUNCTION lr(n int) RETURNS int LANGUAGE sql AS 'WITH x AS (INSERT INTO tv VALUES (n, 1) RETURNING n) SELECT x.n FROM x'",
                "SELECT lr(61)",
                Value::Int(61),
            ),
        ],
    );
}

#[test]
fn output_columns_and_whole_row_relations_take_a_name_before_a_parameter() {
    let engine = tables();
    check(
        &engine,
        &[
            (
                "CREATE FUNCTION ob(p int) RETURNS int LANGUAGE sql AS 'SELECT v AS p FROM t ORDER BY p DESC LIMIT 1'",
                "SELECT ob(0)",
                Value::Int(2),
            ),
            // Inside an expression an output name is not visible, so the name is the parameter.
            (
                "CREATE FUNCTION ob2(p int) RETURNS int LANGUAGE sql AS 'SELECT v AS p FROM t ORDER BY p + 0 DESC, v LIMIT 1'",
                "SELECT ob2(0)",
                Value::Int(1),
            ),
            (
                "CREATE FUNCTION gb(p int) RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM (SELECT v AS p FROM t GROUP BY p) s'",
                "SELECT gb(5)",
                Value::Int(2),
            ),
            (
                "CREATE FUNCTION dn(p int) RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM (SELECT DISTINCT ON (p) v AS p FROM t) s'",
                "SELECT dn(5)",
                Value::Int(2),
            ),
            (
                "CREATE FUNCTION wr(t int) RETURNS text LANGUAGE sql AS 'SELECT t::text FROM t ORDER BY id LIMIT 1'",
                "SELECT wr(3)",
                Value::Str("(10,1)".into()),
            ),
            (
                "CREATE FUNCTION wrs(t int) RETURNS text LANGUAGE sql AS 'SELECT (SELECT t::text) FROM t ORDER BY id LIMIT 1'",
                "SELECT wrs(3)",
                Value::Str("(10,1)".into()),
            ),
        ],
    );
    for mode in ["auto", "force_custom_plan", "force_generic_plan"] {
        sql(&engine, &format!("SET plan_cache_mode = '{mode}'"));
        for parameter in [0, 1, 2, -1, 99, 0, 2] {
            assert_eq!(
                value(&engine, &format!("SELECT ob2({parameter})")),
                Value::Int(1),
            );
        }
    }
}

#[test]
fn each_clause_of_a_data_modifying_statement_sees_its_own_relations() {
    let engine = tables();
    check(
        &engine,
        &[
            // The rows of an INSERT see no relation, not even in a subquery.
            (
                "CREATE FUNCTION iv(id int) RETURNS int LANGUAGE sql AS 'INSERT INTO tv VALUES (id, 1) RETURNING id'",
                "SELECT iv(42)",
                Value::Int(42),
            ),
            (
                "CREATE FUNCTION ivs(id int) RETURNS int LANGUAGE sql AS 'INSERT INTO tv VALUES ((SELECT id), 2) RETURNING id'",
                "SELECT ivs(50)",
                Value::Int(50),
            ),
            (
                "CREATE FUNCTION ir(v int) RETURNS int LANGUAGE sql AS 'INSERT INTO tv VALUES (43, 9) RETURNING v'",
                "SELECT ir(100)",
                Value::Int(9),
            ),
            (
                "CREATE FUNCTION us(v int) RETURNS int LANGUAGE sql AS 'UPDATE tv SET v = v + 1 WHERE id = 43 RETURNING v'",
                "SELECT us(100)",
                Value::Int(10),
            ),
            (
                "CREATE FUNCTION isel(id int) RETURNS int LANGUAGE sql AS 'INSERT INTO tv SELECT id, w FROM src RETURNING id'",
                "SELECT isel(1)",
                Value::Int(7),
            ),
            // A source of UPDATE does not see the target.
            (
                "CREATE FUNCTION uf(v int) RETURNS int LANGUAGE sql AS 'UPDATE tv SET v = s.x FROM (SELECT v AS x) s WHERE tv.id = 7 RETURNING tv.v'",
                "SELECT uf(33)",
                Value::Int(33),
            ),
            // RETURNING sees the sources of UPDATE.
            (
                "CREATE FUNCTION ret(w int) RETURNS int LANGUAGE sql AS 'UPDATE tv SET v = 1 FROM src WHERE tv.id = src.id RETURNING w'",
                "SELECT ret(-1)",
                Value::Int(70),
            ),
            (
                "CREATE FUNCTION oca(id int) RETURNS int LANGUAGE sql AS 'INSERT INTO tv VALUES (id, 5) ON CONFLICT (id) DO UPDATE SET v = 6 RETURNING v'",
                "SELECT oca(7)",
                Value::Int(6),
            ),
            // A MERGE action for an unmatched source row does not see the target.
            (
                "CREATE FUNCTION mns(id int) RETURNS bigint LANGUAGE sql AS 'WITH m AS (MERGE INTO tv USING (SELECT 99 AS k) s ON tv.id = s.k WHEN NOT MATCHED THEN INSERT VALUES (id, 0) RETURNING tv.id) SELECT count(*) FROM m'",
                "SELECT mns(88)",
                Value::Int(1),
            ),
            (
                "CREATE FUNCTION mg(w int) RETURNS int LANGUAGE sql AS 'MERGE INTO tv USING src ON tv.id = src.id WHEN MATCHED THEN UPDATE SET v = w RETURNING v'",
                "SELECT mg(-5)",
                Value::Int(70),
            ),
            (
                "CREATE FUNCTION dl(id int) RETURNS bigint LANGUAGE sql AS 'WITH d AS (DELETE FROM tv WHERE id = id RETURNING 1) SELECT count(*) FROM d'",
                "SELECT dl(-1)",
                Value::Int(5),
            ),
        ],
    );
    // In DO UPDATE both the target and excluded hold the name, so it is ambiguous before it could be the parameter.
    expect(
        &engine,
        "CREATE FUNCTION oc(v int) RETURNS int LANGUAGE sql AS 'INSERT INTO tv VALUES (43, 1) ON CONFLICT (id) DO UPDATE SET v = excluded.v + v RETURNING v'",
        "42702",
        "column reference \"v\" is ambiguous",
    );
}

#[test]
fn a_body_given_as_a_string_resolves_its_names_when_each_statement_runs() {
    let engine = tables();
    sql(
        &engine,
        "CREATE FUNCTION st(n int) RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM t WHERE n = 1'",
    );
    assert_eq!(value(&engine, "SELECT st(1)"), Value::Int(2));
    assert_eq!(value(&engine, "SELECT st(5)"), Value::Int(0));
    sql(&engine, "ALTER TABLE t ADD COLUMN n int DEFAULT 1");
    assert_eq!(value(&engine, "SELECT st(5)"), Value::Int(2));
}

#[test]
fn a_sql_standard_body_resolves_its_names_when_the_routine_is_defined() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("parameters.db");
    {
        let engine = Engine::open(&path).unwrap();
        sql(&engine, "CREATE TABLE t (id int, v int)");
        sql(&engine, "INSERT INTO t VALUES (10, 1), (20, 2)");
        sql(
            &engine,
            "CREATE FUNCTION sb(id int) RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT id FROM t ORDER BY id LIMIT 1; END",
        );
        sql(
            &engine,
            "CREATE FUNCTION sb2(n int) RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT v FROM t ORDER BY v DESC LIMIT 1 OFFSET n - n; END",
        );
        assert_eq!(value(&engine, "SELECT sb(99)"), Value::Int(10));
        assert_eq!(value(&engine, "SELECT sb2(1)"), Value::Int(2));
        sql(&engine, "ALTER TABLE t ADD COLUMN n int DEFAULT 1");
        assert_eq!(value(&engine, "SELECT sb2(1)"), Value::Int(2));
    }
    let engine = Engine::open(&path).unwrap();
    assert_eq!(value(&engine, "SELECT sb2(1)"), Value::Int(2));
}

#[test]
fn output_parameters_are_not_parameters_of_a_sql_body() {
    let engine = tables();
    expect(
        &engine,
        "CREATE PROCEDURE pb(a int, OUT b int) LANGUAGE sql AS 'SELECT b'",
        "42703",
        "column \"b\" does not exist",
    );
    expect(
        &engine,
        "CREATE PROCEDURE pb2(a int, OUT b int) LANGUAGE sql AS 'SELECT $2'",
        "42P02",
        "there is no parameter $2",
    );
    expect(
        &engine,
        "CREATE FUNCTION fb(a int, OUT b int) LANGUAGE sql AS 'SELECT b'",
        "42703",
        "column \"b\" does not exist",
    );
    sql(
        &engine,
        "CREATE PROCEDURE pa(a int, OUT b int, c int) LANGUAGE sql AS 'SELECT a + $2'",
    );
    assert_eq!(value(&engine, "CALL pa(5, NULL, 3)"), Value::Int(8));
    check(
        &engine,
        &[(
            "CREATE FUNCTION fp(a int, INOUT c int) LANGUAGE sql AS 'SELECT a + c'",
            "SELECT fp(1, 2)",
            Value::Int(3),
        )],
    );
}

#[test]
fn a_sql_standard_body_keeps_its_parameters_when_a_relation_takes_their_names() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("recorded.db");
    let calls = "SELECT sbr(15) AS sbr, sbv(15) AS sbv, (SELECT m FROM sbp(7)) AS sbp";
    let expected = |engine: &Engine| {
        let result = sql(engine, calls);
        let row = &result.rows[0];
        assert_eq!(
            (row["sbr"].clone(), row["sbv"].clone(), row["sbp"].clone()),
            (Value::Int(1), Value::Int(1), Value::Int(7))
        );
    };
    {
        let engine = Engine::open(&path).unwrap();
        for statement in [
            "CREATE TABLE t (id int, v int)",
            "INSERT INTO t VALUES (10, 1), (20, 2)",
            "CREATE FUNCTION sbr(n int) RETURNS bigint LANGUAGE sql BEGIN ATOMIC SELECT count(*) FROM t WHERE id > n; END",
            // The derived table keeps the output name the parameter gave its column.
            "CREATE FUNCTION sbp(n int) RETURNS TABLE (m int) LANGUAGE sql BEGIN ATOMIC SELECT s.n FROM (SELECT n) s; END",
            "CREATE VIEW w AS SELECT id FROM t",
            "CREATE FUNCTION sbv(x int) RETURNS bigint LANGUAGE sql BEGIN ATOMIC SELECT count(*) FROM w WHERE id > x; END",
        ] {
            sql(&engine, statement);
        }
        expected(&engine);
        sql(&engine, "ALTER TABLE t RENAME COLUMN v TO n");
        sql(
            &engine,
            "CREATE OR REPLACE VIEW w AS SELECT id, id AS x FROM t",
        );
        expected(&engine);
    }
    expected(&Engine::open(&path).unwrap());
}
