//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::conversion::value_to_string;

/// The independent `PostgreSQL` 18.4 transcript that Engine also replays on every storage provider.
fn reference() -> serde_json::Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/regexp_matches_rows_oracle.expected.json"
    )))
    .unwrap()
}

fn case<'a>(reference: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap_or_else(|| panic!("{id} is missing from the PostgreSQL reference"))
}

/// Every row of one `regexp_matches` call in `PostgreSQL`'s text output.
fn rows(arguments: &[Value], control: &ProductionControl<'_>) -> Result<Vec<String>> {
    let Some(mut cursor) = RegexMatches::new(arguments, DEFAULT_COLLATION, control)? else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    while let Some(row) = cursor.next_row(control)? {
        rows.push(value_to_string(&row)?);
    }
    Ok(rows)
}

/// Compare a result or diagnostic with the one `PostgreSQL` recorded for case `id`.
fn assert_reference<T: std::fmt::Debug + PartialEq>(
    reference: &serde_json::Value,
    id: &str,
    actual: Result<T>,
    expected: impl FnOnce(&serde_json::Value) -> T,
) {
    let case = case(reference, id);
    if case["error"].is_null() {
        let actual = actual.unwrap_or_else(|error| panic!("{id}: {error}"));
        assert_eq!(actual, expected(&case["results"][0]["rows"]), "{id}");
    } else {
        let error = actual.err().unwrap_or_else(|| panic!("{id} must fail"));
        assert_eq!(error.sqlstate(), case["error"]["sqlstate"].as_str(), "{id}");
        assert_eq!(
            error.to_string(),
            case["error"]["message"].as_str().unwrap(),
            "{id}"
        );
        assert_eq!(error.hint(), case["error"]["hint"].as_str(), "{id}");
    }
}

fn first_column(rows: &serde_json::Value) -> Vec<String> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| row[0].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn regexp_matches_rows_match_the_postgresql_reference() {
    let reference = reference();
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (id, arguments) in [
        ("first_match", vec![text("a1b2"), text("[0-9]")]),
        (
            "global_matches",
            vec![text("a1b2"), text("[0-9]"), text("g")],
        ),
        (
            "global_groups",
            vec![text("a1b2"), text("([a-z])([0-9])"), text("g")],
        ),
        ("nonparticipating_group", vec![text("ab"), text("(a)(x)?")]),
        (
            "global_alternation_groups",
            vec![text("ab"), text("(a)|(b)"), text("g")],
        ),
        (
            "global_empty_pattern",
            vec![text("abc"), text(""), text("g")],
        ),
        (
            "global_empty_star",
            vec![text("abc"), text("x*"), text("g")],
        ),
        (
            "global_after_greedy_match",
            vec![text("aab"), text("a*"), text("g")],
        ),
        ("global_anchor", vec![text("abc"), text("^"), text("g")]),
        (
            "global_case_insensitive",
            vec![text("aAa"), text("a"), text("gi")],
        ),
        (
            "global_multibyte",
            vec![text("a\u{e9}b\u{e9}"), text("\u{e9}"), text("g")],
        ),
        (
            "global_multibyte_empty",
            vec![text("\u{e9}\u{e9}"), text(""), text("g")],
        ),
        (
            "global_newline_sensitive",
            vec![text("a\nb\n"), text(".?$"), text("mg")],
        ),
        ("no_match", vec![text("abc"), text("z")]),
        ("invalid_flag", vec![text("abc"), text("b"), text("z")]),
        (
            "invalid_flag_after_global",
            vec![text("abc"), text("b"), text("gz")],
        ),
    ] {
        assert_reference(&reference, id, rows(&arguments, &control), first_column);
        assert_eq!(budget.used(), 0, "{id}");
    }
    // A NULL argument yields no cursor, so the strict set-returning function returns no rows.
    for arguments in [
        vec![Value::Null, text("a")],
        vec![text("abc"), Value::Null],
        vec![text("abc"), text("b"), Value::Null],
    ] {
        assert!(RegexMatches::new(&arguments, DEFAULT_COLLATION, &control)
            .unwrap()
            .is_none());
    }
    assert_eq!(budget.used(), 0);
}

#[test]
fn single_result_functions_reject_global_after_flags_and_before_the_pattern() {
    let reference = reference();
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (id, name, arguments) in [
        (
            "scalar_match",
            "regexp_match",
            vec![text("a1b2"), text("[0-9]")],
        ),
        (
            "scalar_match_global",
            "regexp_match",
            vec![text("abc"), text("b"), text("g")],
        ),
        (
            "scalar_match_invalid_flag_after_global",
            "regexp_match",
            vec![text("abc"), text("b"), text("gz")],
        ),
        (
            "scalar_match_global_before_pattern",
            "regexp_match",
            vec![text("abc"), text("("), text("g")],
        ),
        (
            "like_global",
            "regexp_like",
            vec![text("abc"), text("b"), text("g")],
        ),
        (
            "count_global",
            "regexp_count",
            vec![text("abc"), text("b"), Value::Int(1), text("g")],
        ),
        (
            "count_start_before_global",
            "regexp_count",
            vec![text("abc"), text("b"), Value::Int(0), text("g")],
        ),
        (
            "instr_global",
            "regexp_instr",
            vec![
                text("abc"),
                text("b"),
                Value::Int(1),
                Value::Int(1),
                Value::Int(0),
                text("g"),
            ],
        ),
        (
            "substr_global",
            "regexp_substr",
            vec![
                text("abc"),
                text("b"),
                Value::Int(1),
                Value::Int(1),
                text("g"),
            ],
        ),
        (
            "replace_global",
            "regexp_replace",
            vec![text("aaa"), text("a"), text("b"), text("g")],
        ),
    ] {
        let actual = run(name, &arguments, &control).and_then(|value| value_to_string(&value));
        assert_reference(&reference, id, actual, |rows| {
            rows[0][0].as_str().unwrap().to_owned()
        });
        assert_eq!(budget.used(), 0, "{id}");
    }
}

#[test]
fn regexp_matches_rows_admit_each_row_and_keep_the_cursor_at_a_rejected_one() {
    let token = CancellationToken::new();
    let input = format!("a{}", "b".repeat(2048));
    let arguments = [text(&input), text("a|b+"), text("g")];
    // The cursor finds every match when it is created and keeps its input and match spans under the creating allowance; each row is admitted under the allowance of the control that requests it. Measure the first row under an ample allowance.
    let ample = MemoryBudget::new(1 << 20);
    let ample_control = ProductionControl::new(&ample, &token, &token);
    let mut cursor = RegexMatches::new(&arguments, DEFAULT_COLLATION, &ample_control)
        .unwrap()
        .unwrap();
    let retained = ample.used();
    let first = cursor.next_row(&ample_control).unwrap().unwrap();
    let first_bytes = first.reserved_bytes();
    drop(first);
    drop(cursor);
    assert_eq!(ample.used(), 0);

    // Request rows under an allowance with room for the first row, but not for the 2048-character second row.
    let mut cursor = RegexMatches::new(&arguments, DEFAULT_COLLATION, &ample_control)
        .unwrap()
        .unwrap();
    assert_eq!(ample.used(), retained);
    let limited = MemoryBudget::new(first_bytes + 512);
    let control = ProductionControl::new(&limited, &token, &token);
    let first = cursor.next_row(&control).unwrap().unwrap();
    assert_eq!(value_to_string(&first).unwrap(), "{a}");
    assert_eq!(
        cursor.next_row(&control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(limited.used(), first_bytes);
    assert_eq!(ample.used(), retained);
    assert_eq!(value_to_string(&first).unwrap(), "{a}");
    // The rejected match is produced again under another allowance, and the cursor then ends.
    let second = cursor.next_row(&ample_control).unwrap().unwrap();
    assert_eq!(
        value_to_string(&second).unwrap(),
        format!("{{{}}}", "b".repeat(2048))
    );
    assert!(cursor.next_row(&control).unwrap().is_none());
    drop(second);
    drop(cursor);
    assert_eq!(ample.used(), 0);
    drop(first);
    assert_eq!(limited.used(), 0);
}

#[test]
fn regexp_matches_rows_observe_both_cancellation_scopes() {
    let budget = MemoryBudget::new(64 * 1024);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut cursor = RegexMatches::new(
            &[text("a1b2"), text("[0-9]"), text("g")],
            DEFAULT_COLLATION,
            &control,
        )
        .unwrap()
        .unwrap();
        let first = cursor.next_row(&control).unwrap().unwrap();
        let retained = budget.used();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            cursor.next_row(&control).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(
            RegexMatches::new(&[text("a"), text("a")], DEFAULT_COLLATION, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), retained);
        assert_eq!(value_to_string(&first).unwrap(), "{1}");
        drop(first);
        drop(cursor);
        assert_eq!(budget.used(), 0);
    }
}
