//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::FunctionDispatch;

/// The independent `PostgreSQL` 18.4 transcript that Engine also replays on every storage provider.
fn reference() -> serde_json::Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/substring_pattern_forms_oracle.expected.json"
    )))
    .unwrap()
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
    eval_dispatched_substring(dispatch, args, DEFAULT_COLLATION, control)
        .expect("substring pattern dispatch")
}

/// Compare one direct evaluation with the cell or diagnostic `PostgreSQL` recorded for case `id`.
fn assert_reference(
    reference: &serde_json::Value,
    id: &str,
    (row, column): (usize, usize),
    similar: bool,
    args: &[Value],
    control: &ProductionControl<'_>,
) {
    let case = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap_or_else(|| panic!("{id} is missing from the PostgreSQL reference"));
    let actual = evaluate(similar, args, control);
    if case["error"].is_null() {
        let expected = match &case["results"][0]["rows"][row][column] {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::String(value) => text(value),
            other => panic!("{id}: unexpected reference cell {other}"),
        };
        let actual = actual.unwrap_or_else(|error| panic!("{id}: {error}"));
        assert_eq!(*actual, expected, "{id}");
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
#[expect(
    clippy::too_many_lines,
    reason = "replays every PostgreSQL reference cell of both pattern forms"
)]
fn substring_pattern_forms_match_the_postgresql_reference() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    // Positional and unrelated dispatches belong to other kernels.
    for dispatch in [
        FunctionDispatch::ToBinInt4,
        FunctionDispatch::RandomInt4Range,
    ] {
        assert!(eval_dispatched_substring(
            dispatch,
            &[text("Thomas")],
            DEFAULT_COLLATION,
            &control
        )
        .is_none());
    }
    let reference = reference();
    let posix = |input: &str, pattern: &str| vec![text(input), text(pattern)];
    let sql_form =
        |input: &str, pattern: &str, escape: &str| vec![text(input), text(pattern), text(escape)];
    let null = || Value::Null;
    for (id, cell, similar, args) in [
        ("posix_suffix", (0, 0), false, posix("Thomas", "...$")),
        ("posix_whole_match", (0, 0), false, posix("foobar", "o.b")),
        ("posix_first_group", (0, 0), false, posix("foobar", "o(.)b")),
        (
            "posix_first_of_groups",
            (0, 0),
            false,
            posix("Thomas", "(h)(o)"),
        ),
        (
            "posix_non_capturing",
            (0, 0),
            false,
            posix("Thomas", "(?:h)o"),
        ),
        (
            "posix_unmatched_group",
            (0, 0),
            false,
            posix("Thomas", "h(x)?o"),
        ),
        ("posix_no_match", (0, 0), false, posix("Thomas", "z")),
        ("posix_numeric_pattern", (0, 0), false, posix("Thomas", "2")),
        ("posix_numeric_match", (0, 0), false, posix("Thomas2", "2")),
        ("posix_empty_match", (0, 0), false, posix("abc", "x*")),
        (
            "posix_embedded_options",
            (0, 0),
            false,
            posix("ABC", "(?i)b"),
        ),
        (
            "posix_multibyte",
            (0, 0),
            false,
            posix("a\u{e9}b", "\u{e9}(.)"),
        ),
        ("posix_nulls", (0, 0), false, vec![null(), text("...$")]),
        ("posix_nulls", (0, 1), false, vec![text("Thomas"), null()]),
        ("posix_rows", (1, 0), false, posix("foobar", "o(.)b")),
        ("posix_rows", (2, 0), false, posix("Thomas", "z")),
        // The regular-expression form is strict, so an invalid pattern is never compiled for a NULL input.
        (
            "posix_null_input_invalid_pattern",
            (0, 0),
            false,
            vec![null(), text("(")],
        ),
        (
            "similar_syntax",
            (0, 0),
            true,
            sql_form("Thomas", r#"%#"o_a#"_"#, "#"),
        ),
        (
            "similar_no_match",
            (0, 0),
            true,
            sql_form("Thomas", r#"%#"o_b#"_"#, "#"),
        ),
        (
            "similar_one_separator",
            (0, 0),
            true,
            sql_form("Thomas", r#"#"T%"#, "#"),
        ),
        (
            "similar_no_separator",
            (0, 0),
            true,
            sql_form("Thomas", "T%", "#"),
        ),
        (
            "similar_no_whole_match",
            (0, 0),
            true,
            sql_form("Thomas", "x%", "#"),
        ),
        (
            "similar_unknown_literals",
            (0, 0),
            true,
            sql_form("Thomas", "2", "3"),
        ),
        (
            "similar_empty_escape",
            (0, 0),
            true,
            sql_form("a#b", "a#b", ""),
        ),
        (
            "similar_nulls",
            (0, 0),
            true,
            vec![text("Thomas"), text("T%"), null()],
        ),
        (
            "similar_nulls",
            (0, 1),
            true,
            vec![text("Thomas"), null(), text("#")],
        ),
        (
            "similar_rows",
            (1, 0),
            true,
            sql_form("abc", r#"a#"b#"c"#, "#"),
        ),
        (
            "similar_three_separators",
            (0, 0),
            true,
            sql_form("Thomas", r#"#"T#"o#"%"#, "#"),
        ),
        (
            "similar_long_escape",
            (0, 0),
            true,
            sql_form("Thomas", "%", "##"),
        ),
        // A row's NULL input still runs the translation of the inlined body; only a NULL pattern or escape skips it.
        (
            "similar_null_input_row_escape",
            (0, 0),
            true,
            vec![null(), text("%"), text("##")],
        ),
        (
            "similar_null_input_row_separators",
            (0, 0),
            true,
            vec![null(), text(r#"#"a#"b#"c#""#), text("#")],
        ),
        (
            "similar_null_pattern_row",
            (0, 0),
            true,
            vec![text("a"), null(), text("##")],
        ),
    ] {
        assert_reference(&reference, id, cell, similar, &args, &control);
        assert_eq!(budget.used(), 0, "{id} retains no evaluation state");
    }
}

#[test]
fn substring_pattern_forms_keep_only_their_result_and_release_rejected_output() {
    let budget = MemoryBudget::new(1 << 20);
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
    // The input copies, the compiled pattern and the search fit; copying the equally long whole match exceeds the remaining allowance. The match is longer than the compiled pattern and its search use, so the copy is the allocation that reaches the call's peak.
    let length = 256 * 1024;
    let input = "a".repeat(length);
    for (similar, args) in [
        (false, vec![text(&input), text("a*")]),
        (true, vec![text(&input), text("a%"), text("#")]),
    ] {
        let measured = MemoryBudget::new(1 << 24);
        let measured_control = ProductionControl::new(&measured, &token, &token);
        let held = measured_control.copy_text("earlier value").unwrap();
        drop(evaluate(similar, &args, &measured_control).unwrap());
        drop(held);
        let budget = MemoryBudget::new(measured.peak() - length / 4);
        let control = ProductionControl::new(&budget, &token, &token);
        let held = control.copy_text("earlier value").unwrap();
        let retained = budget.used();
        assert_eq!(
            evaluate(similar, &args, &control).unwrap_err().sqlstate(),
            Some("53200")
        );
        assert!(
            budget.peak() > retained + 4 * length,
            "the input's code points were admitted before the match was refused"
        );
        assert_eq!(budget.used(), retained);
        assert_eq!(&*held, "earlier value");
        drop(held);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn substring_pattern_forms_observe_both_cancellation_scopes() {
    let budget = MemoryBudget::new(1 << 20);
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

#[test]
fn constant_similar_translation_reports_postgresql_planning_errors() {
    let reference = reference();
    let case = |id: &str| {
        reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == id)
            .unwrap()
            .clone()
    };
    let control = ProductionControl::uncontrolled();
    let error = check_constant_similar_translation(&text("%"), &text("##"), &control).unwrap_err();
    let expected = case("similar_planning_error");
    assert_eq!(error.sqlstate(), expected["error"]["sqlstate"].as_str());
    assert_eq!(
        error.to_string(),
        expected["error"]["message"].as_str().unwrap()
    );
    assert_eq!(error.hint(), expected["error"]["hint"].as_str());
    // A NULL operand leaves the strict translation unevaluated, as PostgreSQL folds it to NULL.
    check_constant_similar_translation(&Value::Null, &text("##"), &control).unwrap();
    check_constant_similar_translation(&text("%"), &text("#"), &control).unwrap();
}

#[test]
fn constant_similar_translation_respects_exact_and_shared_workspace_allowances() {
    let pattern_text = "a%_b".repeat(256);
    let pattern = text(&pattern_text);
    let escape = text("#");
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(usize::MAX);
    check_constant_similar_translation(
        &pattern,
        &escape,
        &ProductionControl::new(&memory, &token, &token),
    )
    .unwrap();
    let required = memory.peak();
    assert!(required > 0);
    assert_eq!(memory.used(), 0);
    // Producers may use smaller growth steps under a tight allowance. Admitting only the input bytes must still reject the nonempty translation; the observed full reservation must suffice exactly.
    for available in [0, 1, pattern_text.len() + 1, required, required + 1] {
        let parent = MemoryBudget::new(required + 1);
        let sibling = parent.reserve(required + 1 - available).unwrap();
        let memory = parent.child(required + 1);
        let result = check_constant_similar_translation(
            &pattern,
            &escape,
            &ProductionControl::new(&memory, &token, &token),
        );
        if available < required {
            assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        } else {
            result.unwrap();
        }
        assert_eq!(memory.used(), 0);
        assert!(parent.peak() <= parent.limit());
        assert_eq!(parent.used(), required + 1 - available);
        drop(sibling);
        assert_eq!(parent.used(), 0);
    }
}
