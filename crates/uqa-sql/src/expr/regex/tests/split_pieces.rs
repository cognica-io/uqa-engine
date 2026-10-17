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
        "/../../tests/parity/pg18/regexp_split_functions_oracle.expected.json"
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

/// Every piece of one `regexp_split_to_table` call.
fn pieces(arguments: &[Value], control: &ProductionControl<'_>) -> Result<Vec<String>> {
    let Some(mut split) = RegexSplit::new(
        "regexp_split_to_table",
        arguments,
        DEFAULT_COLLATION,
        control,
    )?
    else {
        return Ok(Vec::new());
    };
    let mut pieces = Vec::new();
    while let Some(piece) = split.next_piece(control)? {
        pieces.push(value_to_string(&piece)?);
    }
    Ok(pieces)
}

/// Compare a result or diagnostic with the one `PostgreSQL` recorded for case `id`, reading the expected result from `column` of every row.
fn assert_reference(
    reference: &serde_json::Value,
    id: &str,
    column: usize,
    actual: Result<Vec<String>>,
) {
    let case = case(reference, id);
    if case["error"].is_null() {
        let expected: Vec<String> = case["results"][0]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row[column].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            actual.unwrap_or_else(|error| panic!("{id}: {error}")),
            expected,
            "{id}"
        );
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

#[test]
fn regexp_split_pieces_match_the_postgresql_reference() {
    let reference = reference();
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (id, arguments) in [
        ("table_plain", vec![text("hello world"), text(r"\s+")]),
        ("table_flags", vec![text("aXbxc"), text("x"), text("i")]),
        ("table_quoted", vec![text("a.b"), text("."), text("q")]),
        ("table_global", vec![text("abc"), text("b"), text("g")]),
        (
            "table_global_before_pattern",
            vec![text("abc"), text("("), text("g")],
        ),
        (
            "table_invalid_flag",
            vec![text("abc"), text("b"), text("z")],
        ),
        (
            "table_invalid_flag_after_global",
            vec![text("abc"), text("b"), text("gz")],
        ),
        ("table_empty_pattern", vec![text("abc"), text("")]),
        ("table_empty_star", vec![text("abc"), text("x*")]),
        ("table_leading_trailing", vec![text(",a,"), text(",")]),
        ("table_whole_match", vec![text("abc"), text("abc")]),
        ("table_greedy_then_empty", vec![text("baaac"), text("a*")]),
        ("table_anchor_start", vec![text("abc"), text("^")]),
        ("table_anchor_end", vec![text("abc"), text("$")]),
        (
            "table_newline_sensitive",
            vec![text("a\nb"), text("$"), text("n")],
        ),
        ("table_multibyte_empty", vec![text("a\u{e9}b"), text("")]),
        ("table_empty_input", vec![text(""), text("x")]),
        (
            "table_captures_ignored",
            vec![text("a1b2c"), text("([0-9])")],
        ),
    ] {
        assert_reference(&reference, id, 0, pieces(&arguments, &control));
        assert_eq!(budget.used(), 0, "{id}");
    }
    let array = |arguments: &[Value]| {
        run("regexp_split_to_array", arguments, &control)
            .and_then(|value| value_to_string(&value))
            .map(|text| vec![text])
    };
    for (id, column, arguments) in [
        ("array_plain", 0, vec![text("hello world"), text(r"\s+")]),
        ("array_flags", 0, vec![text("aXbxc"), text("x"), text("i")]),
        ("array_global", 0, vec![text("abc"), text("b"), text("g")]),
        (
            "array_invalid_flag_after_global",
            0,
            vec![text("abc"), text("b"), text("gz")],
        ),
        ("array_empty_pattern", 0, vec![text("abc"), text("")]),
        ("array_whole_star", 0, vec![text("aaa"), text("a*")]),
        (
            "array_greedy_then_empty",
            0,
            vec![text("baaac"), text("a*")],
        ),
        ("array_empty_input", 0, vec![text(""), text("x")]),
        ("array_whole_match", 0, vec![text("abc"), text("abc")]),
        ("array_multibyte_empty", 0, vec![text("a\u{e9}b"), text("")]),
        ("array_anchors", 0, vec![text("abc"), text("^")]),
        ("array_anchors", 1, vec![text("abc"), text("$")]),
    ] {
        assert_reference(&reference, id, column, array(&arguments));
        assert_eq!(budget.used(), 0, "{id}");
    }
    // Both functions are strict: a NULL argument yields no rows or NULL before any flag is validated.
    for arguments in [
        vec![Value::Null, text("x")],
        vec![text("abc"), Value::Null],
        vec![text("abc"), text("b"), Value::Null],
        vec![Value::Null, text("x"), text("z")],
    ] {
        assert!(RegexSplit::new(
            "regexp_split_to_table",
            &arguments,
            DEFAULT_COLLATION,
            &control
        )
        .unwrap()
        .is_none());
        assert_eq!(
            *run("regexp_split_to_array", &arguments, &control).unwrap(),
            Value::Null
        );
    }
    assert_eq!(budget.used(), 0);
}

#[test]
fn regexp_split_admits_each_piece_and_keeps_the_cursor_before_a_rejected_one() {
    let token = CancellationToken::new();
    let input = format!("a,{}", "b".repeat(2048));
    let arguments = [text(&input), text(",")];
    // Measure the cursor's admitted input and pattern and its first piece under an ample allowance.
    let ample = MemoryBudget::new(1 << 20);
    let ample_control = ProductionControl::new(&ample, &token, &token);
    let mut split = RegexSplit::new(
        "regexp_split_to_table",
        &arguments,
        DEFAULT_COLLATION,
        &ample_control,
    )
    .unwrap()
    .unwrap();
    let retained = ample.used();
    let first = split.next_piece(&ample_control).unwrap().unwrap();
    let first_bytes = first.reserved_bytes();
    drop(first);
    drop(split);
    assert_eq!(ample.used(), 0);

    // Leave room for the cursor and its first piece, but not for the 2048-character second piece.
    let limited = MemoryBudget::new(retained + first_bytes + 512);
    let control = ProductionControl::new(&limited, &token, &token);
    let mut split = RegexSplit::new(
        "regexp_split_to_table",
        &arguments,
        DEFAULT_COLLATION,
        &control,
    )
    .unwrap()
    .unwrap();
    let first = split.next_piece(&control).unwrap().unwrap();
    assert_eq!(*first, text("a"));
    assert_eq!(
        split.next_piece(&control).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(limited.used(), retained + first_bytes);
    // The rejected piece is produced again under another allowance, and the cursor then ends.
    let second = split.next_piece(&ample_control).unwrap().unwrap();
    assert_eq!(*second, text(&"b".repeat(2048)));
    assert!(split.next_piece(&control).unwrap().is_none());
    drop(second);
    assert_eq!(ample.used(), 0);
    drop(first);
    drop(split);
    assert_eq!(limited.used(), 0);

    // The array releases its cursor and partial pieces when its result is rejected.
    let tight = MemoryBudget::new(retained + first_bytes + 512);
    let control = ProductionControl::new(&tight, &token, &token);
    assert_eq!(
        run("regexp_split_to_array", &arguments, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(tight.used(), 0);
}

#[test]
fn regexp_split_observes_both_cancellation_scopes() {
    let budget = MemoryBudget::new(64 * 1024);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let mut split = RegexSplit::new(
            "regexp_split_to_table",
            &[text("a,b"), text(",")],
            DEFAULT_COLLATION,
            &control,
        )
        .unwrap()
        .unwrap();
        let first = split.next_piece(&control).unwrap().unwrap();
        let retained = budget.used();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(
            split.next_piece(&control).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(
            run("regexp_split_to_array", &[text("a,b"), text(",")], &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), retained);
        assert_eq!(*first, text("a"));
        drop(first);
        drop(split);
        assert_eq!(budget.used(), 0);
    }
}
