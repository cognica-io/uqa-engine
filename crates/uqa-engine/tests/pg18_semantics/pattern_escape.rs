//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn assert_pattern_results<const N: usize>(eng: &Engine, cases: [(&str, Value); N]) {
    for (sql, expected) in cases {
        assert_eq!(scalar(eng, sql), expected, "{sql}");
    }
}

#[test]
fn like_and_ilike_escape_clauses_match_postgresql_18() {
    let eng = engine();
    assert_pattern_results(
        &eng,
        [
            ("SELECT 'a_b' LIKE 'a!_b' ESCAPE '!'", Value::Bool(true)),
            ("SELECT 'a%b' LIKE 'a!%b' ESCAPE '!'", Value::Bool(true)),
            (r"SELECT 'a_b' LIKE 'a\_b'", Value::Bool(true)),
            (
                r"SELECT 'a_b' LIKE 'a\_b' ESCAPE ''",
                Value::Bool(false),
            ),
            (
                "SELECT 'a%b' LIKE 'a💣%b' ESCAPE '💣'",
                Value::Bool(true),
            ),
            (
                "SELECT 'A_B' ILIKE 'a!_b' ESCAPE '!'",
                Value::Bool(true),
            ),
            (
                "SELECT 'a%b' ILIKE 'aX%b' ESCAPE 'X'",
                Value::Bool(true),
            ),
            (
                "SELECT 'a_b' NOT LIKE 'a!_b' ESCAPE '!'",
                Value::Bool(false),
            ),
            (
                "SELECT value LIKE pattern ESCAPE escape FROM (VALUES ('a_b', 'a!_b', '!')) AS t(value, pattern, escape)",
                Value::Bool(true),
            ),
            (
                "SELECT 'a' LIKE 'a!' ESCAPE '!'",
                Value::Bool(false),
            ),
            (
                "SELECT 'a_b' LIKE 'a!_b' ESCAPE NULL",
                Value::Null,
            ),
            (
                "SELECT 'a' LIKE NULL::text ESCAPE 'xx'",
                Value::Null,
            ),
        ],
    );
}

#[test]
fn similar_to_escape_clauses_match_postgresql_18() {
    let eng = engine();
    assert_pattern_results(
        &eng,
        [
            (
                "SELECT 'a_b' SIMILAR TO 'a!_b' ESCAPE '!'",
                Value::Bool(true),
            ),
            (
                "SELECT 'a%b' SIMILAR TO 'a!%b' ESCAPE '!'",
                Value::Bool(true),
            ),
            (r"SELECT 'a_b' SIMILAR TO 'a\_b'", Value::Bool(true)),
            (
                r"SELECT 'a_b' SIMILAR TO 'a\_b' ESCAPE ''",
                Value::Bool(false),
            ),
            (
                "SELECT 'a%b' SIMILAR TO 'a💣%b' ESCAPE '💣'",
                Value::Bool(true),
            ),
            ("SELECT '5' SIMILAR TO '!d' ESCAPE '!'", Value::Bool(true)),
            (
                "SELECT 'a|b' SIMILAR TO 'a!|b' ESCAPE '!'",
                Value::Bool(true),
            ),
            (
                "SELECT chr(8) SIMILAR TO '!b' ESCAPE '!'",
                Value::Bool(true),
            ),
            (
                "SELECT chr(92) SIMILAR TO '!B' ESCAPE '!'",
                Value::Bool(true),
            ),
            ("SELECT '[' SIMILAR TO '[[]'", Value::Bool(true)),
            ("SELECT 'a' SIMILAR TO '[^^]'", Value::Bool(true)),
            ("SELECT '^' SIMILAR TO '[!^]' ESCAPE '!'", Value::Bool(true)),
            ("SELECT ']' SIMILAR TO '[]a]'", Value::Bool(true)),
            ("SELECT 'b' SIMILAR TO '[^a]'", Value::Bool(true)),
            ("SELECT '5' SIMILAR TO '[[:digit:]]'", Value::Bool(true)),
            ("SELECT 'a' SIMILAR TO 'a!' ESCAPE '!'", Value::Bool(true)),
            ("SELECT 'a_b' SIMILAR TO 'a!_b' ESCAPE NULL", Value::Null),
        ],
    );
}

#[test]
fn pattern_escape_errors_match_postgresql_18() {
    let eng = engine();
    for sql in [
        "SELECT 'a' LIKE 'a' ESCAPE 'xx'",
        "SELECT 'a' SIMILAR TO 'a' ESCAPE 'xx'",
        "SELECT NULL::text LIKE 'a' ESCAPE 'xx'",
        "SELECT 'ab' LIKE 'a!' ESCAPE '!'",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22025"), "{sql}: {error}");
    }
}
