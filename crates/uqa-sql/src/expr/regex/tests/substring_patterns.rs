//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::FunctionDispatch;

const VALUES_SQL: &str = "SELECT substring(x from p) FROM (VALUES ('Thomas', '...$'), ('foobar', 'o(.)b'), ('Thomas', 'z')) v(x, p)";
const NULL_POSIX_SQL: &str = "SELECT substring(NULL from '...$'), substring('Thomas' from NULL)";
const NULL_SIMILAR_SQL: &str = "SELECT substring('Thomas' similar 'T%' escape NULL), substring('Thomas' similar NULL escape '#')";

/// The independent `PostgreSQL` 18.4 transcript that Engine also replays on every storage provider.
fn reference() -> serde_json::Value {
    serde_json::from_str(include_str!("substring-postgresql.json")).unwrap()
}

/// Evaluate through the dispatched entry point that scalar execution uses.
fn evaluate(
    similar: bool,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let dispatch = if similar {
        FunctionDispatch::SimilarSubstring
    } else {
        FunctionDispatch::RegexSubstring
    };
    eval_dispatched_substring(dispatch, args, control).expect("substring pattern dispatch")
}

/// Compare one direct evaluation with the cell or diagnostic `PostgreSQL` recorded for `sql`.
fn assert_reference(
    reference: &serde_json::Value,
    sql: &str,
    (row, column): (usize, usize),
    similar: bool,
    args: &[Value],
    control: &ProductionControl<'_>,
) {
    let case = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["sql"] == sql)
        .unwrap_or_else(|| panic!("{sql} is missing from the PostgreSQL reference"));
    let actual = evaluate(similar, args, control);
    if case["error"].is_null() {
        let expected = match &case["results"][0]["rows"][row][column] {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::String(value) => text(value),
            other => panic!("{sql}: unexpected reference cell {other}"),
        };
        let actual = actual.unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(*actual, expected, "{sql}");
    } else {
        let error = actual.err().unwrap_or_else(|| panic!("{sql} must fail"));
        assert_eq!(
            error.sqlstate(),
            case["error"]["sqlstate"].as_str(),
            "{sql}"
        );
        assert_eq!(
            error.to_string(),
            case["error"]["message"].as_str().unwrap(),
            "{sql}"
        );
        assert_eq!(error.hint(), case["error"]["hint"].as_str(), "{sql}");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "replays every PostgreSQL reference cell of both pattern forms"
)]
fn substring_pattern_forms_match_the_postgresql_reference() {
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    // Positional and unrelated dispatches belong to other kernels.
    for dispatch in [
        FunctionDispatch::ToBinInt4,
        FunctionDispatch::RandomInt4Range,
    ] {
        assert!(eval_dispatched_substring(dispatch, &[text("Thomas")], &control).is_none());
    }
    let reference = reference();
    let posix = |input: &str, pattern: &str| vec![text(input), text(pattern)];
    let sql_pattern =
        |input: &str, pattern: &str, escape: &str| vec![text(input), text(pattern), text(escape)];
    for (sql, cell, similar, args) in [
        (
            "SELECT substring('Thomas' from '...$')",
            (0, 0),
            false,
            posix("Thomas", "...$"),
        ),
        (
            "SELECT substring('foobar' from 'o.b')",
            (0, 0),
            false,
            posix("foobar", "o.b"),
        ),
        (
            "SELECT substring('foobar' from 'o(.)b')",
            (0, 0),
            false,
            posix("foobar", "o(.)b"),
        ),
        (
            "SELECT substring('Thomas' from '(h)(o)')",
            (0, 0),
            false,
            posix("Thomas", "(h)(o)"),
        ),
        (
            "SELECT substring('Thomas' from '(?:h)o')",
            (0, 0),
            false,
            posix("Thomas", "(?:h)o"),
        ),
        (
            "SELECT substring('Thomas' from 'h(x)?o')",
            (0, 0),
            false,
            posix("Thomas", "h(x)?o"),
        ),
        (
            "SELECT substring('Thomas' from 'z')",
            (0, 0),
            false,
            posix("Thomas", "z"),
        ),
        (
            "SELECT substring('Thomas' from '2')",
            (0, 0),
            false,
            posix("Thomas", "2"),
        ),
        (
            "SELECT substring('Thomas2' from '2')",
            (0, 0),
            false,
            posix("Thomas2", "2"),
        ),
        (
            "SELECT substring('abc' from 'x*')",
            (0, 0),
            false,
            posix("abc", "x*"),
        ),
        (
            "SELECT substring('ABC' from '(?i)b')",
            (0, 0),
            false,
            posix("ABC", "(?i)b"),
        ),
        (
            "SELECT substring('a' || chr(233) || 'b' from chr(233) || '(.)')",
            (0, 0),
            false,
            posix("a\u{e9}b", "\u{e9}(.)"),
        ),
        (
            NULL_POSIX_SQL,
            (0, 0),
            false,
            vec![Value::Null, text("...$")],
        ),
        (
            NULL_POSIX_SQL,
            (0, 1),
            false,
            vec![text("Thomas"), Value::Null],
        ),
        (VALUES_SQL, (1, 0), false, posix("foobar", "o(.)b")),
        (VALUES_SQL, (2, 0), false, posix("Thomas", "z")),
        (
            r#"SELECT substring('Thomas' similar '%#"o_a#"_' escape '#')"#,
            (0, 0),
            true,
            sql_pattern("Thomas", r#"%#"o_a#"_"#, "#"),
        ),
        (
            r#"SELECT substring('Thomas' similar '%#"o_b#"_' escape '#')"#,
            (0, 0),
            true,
            sql_pattern("Thomas", r#"%#"o_b#"_"#, "#"),
        ),
        (
            r#"SELECT substring('Thomas' similar '#"T%' escape '#')"#,
            (0, 0),
            true,
            sql_pattern("Thomas", r#"#"T%"#, "#"),
        ),
        (
            "SELECT substring('Thomas' similar 'T%' escape '#')",
            (0, 0),
            true,
            sql_pattern("Thomas", "T%", "#"),
        ),
        (
            "SELECT substring('Thomas' similar 'x%' escape '#')",
            (0, 0),
            true,
            sql_pattern("Thomas", "x%", "#"),
        ),
        (
            "SELECT substring('Thomas' from '2' for '3')",
            (0, 0),
            true,
            sql_pattern("Thomas", "2", "3"),
        ),
        (
            "SELECT substring('a#b' similar 'a#b' escape '')",
            (0, 0),
            true,
            sql_pattern("a#b", "a#b", ""),
        ),
        (
            NULL_SIMILAR_SQL,
            (0, 0),
            true,
            vec![text("Thomas"), text("T%"), Value::Null],
        ),
        (
            NULL_SIMILAR_SQL,
            (0, 1),
            true,
            vec![text("Thomas"), Value::Null, text("#")],
        ),
        (
            r#"SELECT substring('Thomas' similar '#"T#"o#"%' escape '#')"#,
            (0, 0),
            true,
            sql_pattern("Thomas", r#"#"T#"o#"%"#, "#"),
        ),
        (
            "SELECT substring('Thomas' similar '%' escape '##')",
            (0, 0),
            true,
            sql_pattern("Thomas", "%", "##"),
        ),
    ] {
        assert_reference(&reference, sql, cell, similar, &args, &control);
        assert_eq!(budget.used(), 0, "{sql} retains no evaluation state");
    }
}

#[test]
fn substring_pattern_forms_keep_only_their_result_and_release_rejected_output() {
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (similar, args) in [
        (false, vec![text("Thomas"), text("o(m)a")]),
        (true, vec![text("Thomas"), text(r#"%#"o_a#"_"#), text("#")]),
    ] {
        let result = evaluate(similar, &args, &control).unwrap();
        assert_eq!(
            budget.used(),
            result.reserved_bytes(),
            "only the SQL result survives"
        );
        drop(result);
        assert_eq!(budget.used(), 0);
    }
    let held = control.copy_text("earlier value").unwrap();
    let retained = budget.used();
    // The input copy fits; copying the equally long whole match exceeds the remaining allowance.
    let input = "a".repeat(10 * 1024);
    for (similar, args) in [
        (false, vec![text(&input), text("a*")]),
        (true, vec![text(&input), text("a%"), text("#")]),
    ] {
        assert_eq!(
            evaluate(similar, &args, &control).unwrap_err().sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), retained);
    }
    assert_eq!(&*held, "earlier value");
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn substring_pattern_forms_observe_both_cancellation_scopes() {
    let budget = MemoryBudget::new(8192);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let held = evaluate(false, &[text("before"), text(".*")], &control).unwrap();
        let retained = budget.used();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        for (similar, args) in [
            (false, vec![text("later"), text(".*")]),
            (true, vec![text("later"), text("%"), text("#")]),
        ] {
            assert_eq!(
                evaluate(similar, &args, &control).unwrap_err().sqlstate(),
                Some("57014")
            );
            assert_eq!(budget.used(), retained);
        }
        assert_eq!(*held, text("before"));
        drop(held);
        assert_eq!(budget.used(), 0);
    }
}
