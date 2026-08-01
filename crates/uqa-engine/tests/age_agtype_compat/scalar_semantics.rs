//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

// ---------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------

#[test]
fn arithmetic_matches_age() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('arith')");
    let g = "arith";
    assert_agtype(&eng, g, "RETURN 1/3", "0");
    assert_agtype(&eng, g, "RETURN -7/3", "-2");
    assert_agtype(&eng, g, "RETURN 7%3", "1");
    assert_agtype(&eng, g, "RETURN -7%3", "-1");
    assert_agtype(&eng, g, "RETURN 7%-3", "1");
    assert_agtype(&eng, g, "RETURN 7.5%2", "1.5");
    assert_agtype(&eng, g, "RETURN -7.5%2", "-1.5");
    // `^` is ALWAYS float; unary minus binds tighter; left-assoc.
    assert_agtype(&eng, g, "RETURN 2^2", "4.0");
    assert_agtype(&eng, g, "RETURN 2^-1", "0.5");
    assert_agtype(&eng, g, "RETURN -2^2", "4.0");
    assert_agtype(&eng, g, "RETURN 2^3^2", "64.0");
    assert_cypher_error(&eng, g, "RETURN 1/0", "division by zero");
    assert_cypher_error(&eng, g, "RETURN 1.0/0.0", "division by zero");
    assert_cypher_error(&eng, g, "RETURN 0.0/0.0", "division by zero");
    // AGE quirks verified on 1.6.0: n % 0 returns n; float % 0 is NaN.
    assert_agtype(&eng, g, "RETURN 5%0", "5");
    assert_agtype(&eng, g, "RETURN 1.5%0.0", "NaN");
    // int64 arithmetic wraps.
    assert_agtype(
        &eng,
        g,
        "RETURN 9223372036854775807 + 1",
        "-9223372036854775808",
    );
    assert_sql_null(&eng, g, "RETURN sqrt(-1)");
    assert_sql_null(&eng, g, "RETURN log(0)");
    assert_sql_null(&eng, g, "RETURN log(-1)");
    assert_agtype(&eng, g, "RETURN sqrt(4)", "2.0");
    assert_agtype(&eng, g, "RETURN abs(-3)", "3");
    assert_agtype(&eng, g, "RETURN abs(-3.5)", "3.5");
    assert_agtype(&eng, g, "RETURN sign(-2)", "-1");
    assert_agtype(&eng, g, "RETURN sign(0)", "0");
    assert_agtype(&eng, g, "RETURN sign(2.5)", "1");
    assert_agtype(&eng, g, "RETURN ceil(0.1)", "1.0");
    assert_agtype(&eng, g, "RETURN ceil(2)", "2.0");
    assert_agtype(&eng, g, "RETURN floor(0.9)", "0.0");
    assert_agtype(&eng, g, "RETURN round(0.5)", "1.0");
    assert_agtype(&eng, g, "RETURN round(-0.5)", "-1.0");
    assert_agtype(&eng, g, "RETURN toFloat(1)/3", "0.3333333333333333");
    assert_agtype(&eng, g, "RETURN 2.0^63", "9.223372036854776e+18");
    assert_agtype(&eng, g, "RETURN 0.1 + 0.2", "0.30000000000000004");
    // Mixed int/float promotes to float.
    assert_agtype(&eng, g, "RETURN 3 + 4.0", "7.0");
    assert_agtype(&eng, g, "RETURN 5.0 / 2", "2.5");
    // Float rendering matrix.
    assert_agtype(&eng, g, "RETURN 100.0", "100.0");
    assert_agtype(&eng, g, "RETURN -0.0", "-0.0");
    assert_agtype(&eng, g, "RETURN 1e15", "1e+15");
    assert_agtype(&eng, g, "RETURN 1e14", "100000000000000.0");
    assert_agtype(&eng, g, "RETURN 0.0001", "0.0001");
    assert_agtype(&eng, g, "RETURN 0.00001", "1e-05");
    assert_agtype(&eng, g, "RETURN 1e100", "1e+100");
    assert_agtype(&eng, g, "RETURN 1e308 * 10", "Infinity");
    assert_agtype(&eng, g, "RETURN -1e308 * 10", "-Infinity");
}

// ---------------------------------------------------------------------
// ORDER BY type ordering
// ---------------------------------------------------------------------

#[test]
fn order_by_uses_agtype_type_order() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('ordering')");
    // Ascending: object < list < string < bool < number < null.
    let asc = agtype_rows(
        &eng,
        "ordering",
        "UNWIND [1,'a',null,2.5,true,[1],{x:1}] AS v RETURN v ORDER BY v",
    );
    assert_eq!(
        asc,
        vec![
            Some("{\"x\": 1}".to_string()),
            Some("[1]".to_string()),
            Some("\"a\"".to_string()),
            Some("true".to_string()),
            Some("1".to_string()),
            Some("2.5".to_string()),
            None,
        ]
    );
    // Descending is the exact reverse (null first).
    let desc = agtype_rows(
        &eng,
        "ordering",
        "UNWIND [1,'a',null,2.5,true,[1],{x:1}] AS v RETURN v ORDER BY v DESC",
    );
    assert_eq!(
        desc,
        vec![
            None,
            Some("2.5".to_string()),
            Some("1".to_string()),
            Some("true".to_string()),
            Some("\"a\"".to_string()),
            Some("[1]".to_string()),
            Some("{\"x\": 1}".to_string()),
        ]
    );
    // Comparison operators use the same total order.
    assert_agtype(&eng, "ordering", "RETURN 1 < 'a'", "false");
    assert_agtype(&eng, "ordering", "RETURN 'a' < 1", "true");
    assert_agtype(&eng, "ordering", "RETURN true < 1", "true");
    assert_agtype(&eng, "ordering", "RETURN [1] < 'a'", "true");
    assert_agtype(&eng, "ordering", "RETURN {a:1} < [1]", "true");
    assert_agtype(&eng, "ordering", "RETURN 'ab' < 'b'", "true");
    assert_agtype(&eng, "ordering", "RETURN [1,2] < [1,2,3]", "true");
    assert_agtype(&eng, "ordering", "RETURN 1 = 1.0", "true");
    assert_agtype(&eng, "ordering", "RETURN 'a' = 1", "false");
    // Comparisons chain left-associatively: (1 < 2) < 3.
    assert_agtype(&eng, "ordering", "RETURN 1 < 2 < 3", "true");
}

// ---------------------------------------------------------------------
// NULL semantics
// ---------------------------------------------------------------------

#[test]
fn null_semantics_match_age() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('nulls')");
    let g = "nulls";
    assert_sql_null(&eng, g, "RETURN null");
    assert_sql_null(&eng, g, "RETURN null = null");
    assert_sql_null(&eng, g, "RETURN null <> 1");
    assert_sql_null(&eng, g, "RETURN null + 1");
    assert_sql_null(&eng, g, "RETURN 1 < null");
    assert_sql_null(&eng, g, "RETURN -null");
    assert_agtype(&eng, g, "RETURN null IS NULL", "true");
    assert_agtype(&eng, g, "RETURN [null][0] IS NULL", "true");
    // Nested null renders as `null`.
    assert_agtype(&eng, g, "RETURN [null, 1]", "[null, 1]");
    // Three-valued logic (strict boolean operands).
    assert_sql_null(&eng, g, "RETURN true AND null");
    assert_agtype(&eng, g, "RETURN false AND null", "false");
    assert_agtype(&eng, g, "RETURN true OR null", "true");
    assert_sql_null(&eng, g, "RETURN false OR null");
    assert_sql_null(&eng, g, "RETURN NOT null");
    assert_sql_null(&eng, g, "RETURN null XOR true");
    assert_cypher_error(
        &eng,
        g,
        "RETURN 1 AND true",
        "cannot cast agtype integer to type boolean",
    );
    // WHERE requires a boolean (or null, which filters). The graph
    // needs a row for the per-row evaluation to hit the cast.
    exec(
        &eng,
        "SELECT * FROM cypher('nulls', $$ CREATE (:Thing) $$) AS (v agtype)",
    );
    assert_cypher_error(
        &eng,
        g,
        "MATCH (n) WHERE 1 RETURN n",
        "cannot cast agtype integer to type boolean",
    );
    assert_eq!(
        agtype_rows(&eng, g, "UNWIND [1] AS x WITH x WHERE null RETURN x").len(),
        0
    );
    assert_sql_null(&eng, g, "RETURN coalesce(null, null)");
    assert_agtype(&eng, g, "RETURN coalesce(null, 5)", "5");
    assert_sql_null(&eng, g, "RETURN size(null)");
    assert_sql_null(&eng, g, "RETURN toUpper(null)");
    assert_sql_null(&eng, g, "RETURN 'abc' STARTS WITH null");
    assert_sql_null(&eng, g, "RETURN id(null)");
    assert_sql_null(&eng, g, "RETURN type(null)");
    assert_sql_null(&eng, g, "RETURN startNode(null)");
}

// ---------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------

#[test]
fn list_semantics_match_age() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('lists')");
    let g = "lists";
    // Slices are end-exclusive; negative offsets wrap; bounds clamp.
    assert_agtype(&eng, g, "RETURN [1,2,3][0..2]", "[1, 2]");
    assert_agtype(&eng, g, "RETURN [1,2,3][-1]", "3");
    assert_agtype(&eng, g, "RETURN [1,2,3][-2..]", "[2, 3]");
    assert_agtype(&eng, g, "RETURN [1,2,3][..2]", "[1, 2]");
    assert_agtype(&eng, g, "RETURN [1,2,3][0..10]", "[1, 2, 3]");
    assert_agtype(&eng, g, "RETURN [1,2,3][2..0]", "[]");
    assert_agtype(&eng, g, "RETURN [1,2,3][-10..-1]", "[1, 2]");
    assert_sql_null(&eng, g, "RETURN [1,2,3][5]");
    // range() is end-INCLUSIVE and honors negative steps.
    assert_agtype(&eng, g, "RETURN range(0,10,3)", "[0, 3, 6, 9]");
    assert_agtype(&eng, g, "RETURN range(0,3)", "[0, 1, 2, 3]");
    assert_agtype(&eng, g, "RETURN range(3,0,-1)", "[3, 2, 1, 0]");
    assert_cypher_error(
        &eng,
        g,
        "RETURN range(1,5,0)",
        "range(): step cannot be zero",
    );
    // List comprehensions.
    assert_agtype(
        &eng,
        g,
        "RETURN [x IN [1,2,3] WHERE x > 1 | x * 10]",
        "[20, 30]",
    );
    assert_agtype(&eng, g, "RETURN [x IN [1,2,3] WHERE x > 1]", "[2, 3]");
    assert_agtype(&eng, g, "RETURN [x IN [1,2,3] | x * 10]", "[10, 20, 30]");
    // IN with null-aware membership.
    assert_agtype(&eng, g, "RETURN 1 IN [1,2]", "true");
    assert_agtype(&eng, g, "RETURN 3 IN [1,2]", "false");
    assert_agtype(&eng, g, "RETURN 1 IN [1, null]", "true");
    assert_sql_null(&eng, g, "RETURN 3 IN [1, null]");
    assert_sql_null(&eng, g, "RETURN null IN [1,2]");
    assert_sql_null(&eng, g, "RETURN 1 IN null");
    assert_cypher_error(&eng, g, "RETURN 1 IN 5", "object of IN must be a list");
    // Concatenation: list + list, list + scalar, scalar + list.
    assert_agtype(&eng, g, "RETURN [1,2] + [3]", "[1, 2, 3]");
    assert_agtype(&eng, g, "RETURN [1,2] + 3", "[1, 2, 3]");
    assert_agtype(&eng, g, "RETURN 2 + [1]", "[2, 1]");
    // Map merge.
    assert_agtype(&eng, g, "RETURN {a: 1} + {b: 2}", "{\"a\": 1, \"b\": 2}");
    // head/last/tail.
    assert_agtype(&eng, g, "RETURN head([1,2,3])", "1");
    assert_agtype(&eng, g, "RETURN last([1,2,3])", "3");
    assert_agtype(&eng, g, "RETURN tail([1,2,3])", "[2, 3]");
    assert_agtype(&eng, g, "RETURN reverse([1,2,3])", "[3, 2, 1]");
    // Map literals with nesting render in JSONB key order.
    assert_agtype(
        &eng,
        g,
        "RETURN {x: 1, y: 'a', nested: {z: [1,2]}}",
        "{\"x\": 1, \"y\": \"a\", \"nested\": {\"z\": [1, 2]}}",
    );
    assert_agtype(&eng, g, "RETURN {a: {b: 2}}.a.b", "2");
    assert_agtype(&eng, g, "RETURN {a: 1}['a']", "1");
    assert_sql_null(&eng, g, "RETURN {a: 1}.missing");
    assert_cypher_error(&eng, g, "RETURN 'hello'[0..2]", "slice must access a list");
    assert_cypher_error(
        &eng,
        g,
        "WITH 5 AS v RETURN v.x",
        "scalar object must be a vertex or edge",
    );
    // UNWIND over a bare scalar yields that scalar.
    assert_agtype(&eng, g, "UNWIND 5 AS x RETURN x", "5");
}

// ---------------------------------------------------------------------
// Strings
// ---------------------------------------------------------------------

#[test]
fn string_semantics_match_age() {
    let eng = Engine::new();
    exec(&eng, "SELECT create_graph('strings')");
    let g = "strings";
    assert_agtype(&eng, g, "RETURN toUpper('aB')", "\"AB\"");
    assert_agtype(&eng, g, "RETURN toLower('aB')", "\"ab\"");
    // substring is 0-based and character-oriented.
    assert_agtype(&eng, g, "RETURN substring('hello', 1, 3)", "\"ell\"");
    assert_agtype(&eng, g, "RETURN substring('hello', 1)", "\"ello\"");
    assert_agtype(&eng, g, "RETURN substring('hello', 10)", "\"\"");
    assert_cypher_error(
        &eng,
        g,
        "RETURN substring('hello', -1)",
        "substring() negative values are not supported for offset or length",
    );
    assert_agtype(
        &eng,
        g,
        "RETURN split('a,b,c', ',')",
        "[\"a\", \"b\", \"c\"]",
    );
    assert_agtype(&eng, g, "RETURN replace('aaa', 'a', 'b')", "\"bbb\"");
    assert_agtype(&eng, g, "RETURN trim('  x ')", "\"x\"");
    assert_agtype(&eng, g, "RETURN lTrim('  x ')", "\"x \"");
    assert_agtype(&eng, g, "RETURN rTrim('  x ')", "\"  x\"");
    assert_agtype(&eng, g, "RETURN left('hello', 2)", "\"he\"");
    assert_agtype(&eng, g, "RETURN right('hello', 2)", "\"lo\"");
    assert_cypher_error(
        &eng,
        g,
        "RETURN left('abc', -1)",
        "left() negative values are not supported for length",
    );
    assert_agtype(&eng, g, "RETURN reverse('abc')", "\"cba\"");
    // `=~` is an UNANCHORED regex search (PostgreSQL `~` semantics).
    assert_agtype(&eng, g, "RETURN 'abc' =~ 'a.*'", "true");
    assert_agtype(&eng, g, "RETURN 'abc' =~ 'b'", "true");
    assert_agtype(&eng, g, "RETURN 'abc' =~ '^b'", "false");
    assert_agtype(&eng, g, "RETURN 'abc' =~ 'd'", "false");
    assert_agtype(&eng, g, "RETURN 'Abc' =~ '(?i)a.*'", "true");
    assert_agtype(&eng, g, "RETURN 'abc' STARTS WITH 'a'", "true");
    assert_agtype(&eng, g, "RETURN 'abc' ENDS WITH 'c'", "true");
    assert_agtype(&eng, g, "RETURN 'abc' CONTAINS 'b'", "true");
    // Non-string operands compare false (not an error).
    assert_agtype(&eng, g, "RETURN 'abc' STARTS WITH 1", "false");
    // String concatenation coerces scalars (AGE renders bools empty).
    assert_agtype(&eng, g, "RETURN 'a' + 'b'", "\"ab\"");
    assert_agtype(&eng, g, "RETURN 'a' + 1", "\"a1\"");
    assert_agtype(&eng, g, "RETURN 1 + 'a'", "\"1a\"");
    assert_agtype(&eng, g, "RETURN 'a' + 1.5", "\"a1.5\"");
    assert_agtype(&eng, g, "RETURN 'a' + true", "\"a\"");
    // size() counts BYTES on strings, elements on lists.
    assert_agtype(&eng, g, "RETURN size('hello')", "5");
    assert_agtype(&eng, g, "RETURN size([1,2])", "2");
    assert_cypher_error(
        &eng,
        g,
        "RETURN size({a: 1})",
        "size() unsupported argument",
    );
    // Conversions.
    assert_agtype(&eng, g, "RETURN toInteger('42')", "42");
    assert_agtype(&eng, g, "RETURN toInteger('4.9')", "4");
    assert_agtype(&eng, g, "RETURN toInteger(4.9)", "4");
    assert_agtype(&eng, g, "RETURN toInteger(-4.9)", "-4");
    assert_sql_null(&eng, g, "RETURN toInteger('abc')");
    assert_agtype(&eng, g, "RETURN toFloat('1.5')", "1.5");
    assert_agtype(&eng, g, "RETURN toFloat(1)", "1.0");
    assert_sql_null(&eng, g, "RETURN toFloat('abc')");
    assert_agtype(&eng, g, "RETURN toBoolean('true')", "true");
    assert_agtype(&eng, g, "RETURN toBoolean('TRUE')", "true");
    assert_agtype(&eng, g, "RETURN toString(1)", "\"1\"");
    assert_agtype(&eng, g, "RETURN toString(1.5)", "\"1.5\"");
    // toString on floats uses raw float8out (no `.0` suffix).
    assert_agtype(&eng, g, "RETURN toString(1.0)", "\"1\"");
    assert_agtype(&eng, g, "RETURN toString(true)", "\"true\"");
    // Wrong-type arguments raise AGE's ordinal-tagged errors.
    assert_cypher_error(
        &eng,
        g,
        "RETURN toUpper(5)",
        "toUpper() unsupported argument agtype 3",
    );
    assert_cypher_error(
        &eng,
        g,
        "RETURN abs('x')",
        "abs() unsupported argument agtype 1",
    );
    // JSON escaping in rendered strings.
    assert_agtype(&eng, g, "RETURN 'a\"b\\n'", "\"a\\\"b\\n\"");
}
