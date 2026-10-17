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
        "/../../tests/parity/pg18/regexp_search_loop_oracle.expected.json"
    )))
    .unwrap()
}

/// Evaluate one scalar call and compare it with the cell or diagnostic `PostgreSQL` recorded for case `id`.
fn assert_reference(
    reference: &serde_json::Value,
    id: &str,
    name: &str,
    arguments: &[Value],
    control: &ProductionControl<'_>,
) {
    let case = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap_or_else(|| panic!("{id} is missing from the PostgreSQL reference"));
    let actual = run(name, arguments, control);
    if case["error"].is_null() {
        let actual = actual.unwrap_or_else(|error| panic!("{id}: {error}"));
        let expected = &case["results"][0]["rows"][0][0];
        match expected {
            serde_json::Value::Null => assert_eq!(*actual, Value::Null, "{id}"),
            serde_json::Value::String(expected) => {
                assert_eq!(&value_to_string(&actual).unwrap(), expected, "{id}");
            }
            other => panic!("{id}: unexpected reference cell {other}"),
        }
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
    reason = "replays every PostgreSQL reference case of the four search-loop functions"
)]
fn search_loop_functions_match_the_postgresql_reference() {
    let reference = reference();
    let budget = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let int = |value: i64| Value::Int(value);
    let e_acute = "\u{e9}";
    for (id, name, arguments) in [
        (
            "count_empty_after_match",
            "regexp_count",
            vec![text("aab"), text("a*")],
        ),
        (
            "count_alternation_empty",
            "regexp_count",
            vec![text("ab"), text("a|")],
        ),
        (
            "count_empty_pattern",
            "regexp_count",
            vec![text("abc"), text("")],
        ),
        (
            "count_start_at_end",
            "regexp_count",
            vec![text("abc"), text(""), int(4)],
        ),
        (
            "count_start_past_end",
            "regexp_count",
            vec![text("abc"), text(""), int(5)],
        ),
        (
            "count_anchor_with_start",
            "regexp_count",
            vec![text("ab"), text("^b"), int(2)],
        ),
        (
            "count_absolute_anchor_with_start",
            "regexp_count",
            vec![text("ab"), text(r"\Ab"), int(2)],
        ),
        (
            "count_multibyte_start",
            "regexp_count",
            vec![text(&format!("{e_acute}a{e_acute}")), text(e_acute), int(2)],
        ),
        (
            "instr_second_empty",
            "regexp_instr",
            vec![text("aab"), text("a*"), int(1), int(2)],
        ),
        (
            "instr_third_empty",
            "regexp_instr",
            vec![text("aab"), text("a*"), int(1), int(3)],
        ),
        (
            "instr_end_option",
            "regexp_instr",
            vec![text("aab"), text("a*"), int(1), int(1), int(1)],
        ),
        (
            "instr_anchor_with_start",
            "regexp_instr",
            vec![text("ab"), text("^b"), int(2)],
        ),
        (
            "instr_start_at_end",
            "regexp_instr",
            vec![text("abc"), text(""), int(4)],
        ),
        (
            "instr_whole_match_subexpression",
            "regexp_instr",
            vec![
                text("abc"),
                text("b"),
                int(1),
                int(1),
                int(0),
                text(""),
                int(1),
            ],
        ),
        (
            "instr_subexpression_beyond",
            "regexp_instr",
            vec![
                text("abc"),
                text("b"),
                int(1),
                int(1),
                int(0),
                text(""),
                int(2),
            ],
        ),
        (
            "instr_unmatched_group",
            "regexp_instr",
            vec![
                text("abc"),
                text("(x)?b"),
                int(1),
                int(1),
                int(0),
                text(""),
                int(1),
            ],
        ),
        (
            "instr_multibyte_position",
            "regexp_instr",
            vec![text(&format!("{e_acute}ab")), text("b"), int(2)],
        ),
        (
            "instr_multibyte_empty",
            "regexp_instr",
            vec![
                text(&format!("{e_acute}{e_acute}")),
                text(""),
                int(1),
                int(3),
            ],
        ),
        (
            "instr_zero_n",
            "regexp_instr",
            vec![text("abc"), text("b"), int(1), int(0)],
        ),
        (
            "substr_third_empty",
            "regexp_substr",
            vec![text("aab"), text("a*"), int(1), int(3)],
        ),
        (
            "substr_anchor_with_start",
            "regexp_substr",
            vec![text("ab"), text("^b"), int(2)],
        ),
        (
            "substr_start_at_end",
            "regexp_substr",
            vec![text("abc"), text(""), int(4)],
        ),
        (
            "substr_whole_match_subexpression",
            "regexp_substr",
            vec![text("abc"), text("b"), int(1), int(1), text(""), int(1)],
        ),
        (
            "substr_group",
            "regexp_substr",
            vec![
                text("a1b2"),
                text("([a-z])([0-9])"),
                int(1),
                int(2),
                text(""),
                int(2),
            ],
        ),
        (
            "substr_zero_n",
            "regexp_substr",
            vec![text("abc"), text("b"), int(1), int(0)],
        ),
        (
            "replace_global_empty_after_match",
            "regexp_replace",
            vec![text("aab"), text("a*"), text("X"), text("g")],
        ),
        (
            "replace_nth_empty",
            "regexp_replace",
            vec![text("aab"), text("a*"), text("X"), int(1), int(2)],
        ),
        (
            "replace_anchor_with_start",
            "regexp_replace",
            vec![text("ab"), text("^b"), text("X"), int(2)],
        ),
        (
            "replace_start_keeps_prefix",
            "regexp_replace",
            vec![text("abab"), text("b"), text("X"), int(3)],
        ),
        (
            "replace_start_at_end",
            "regexp_replace",
            vec![text("abc"), text(""), text("X"), int(4)],
        ),
        (
            "replace_start_past_end",
            "regexp_replace",
            vec![text("abc"), text(""), text("X"), int(5)],
        ),
        (
            "replace_pattern_compiled_past_start",
            "regexp_replace",
            vec![text("a"), text("["), text("x"), int(3)],
        ),
        (
            "replace_backreferences_global",
            "regexp_replace",
            vec![
                text("a1b2"),
                text("([a-z])([0-9])"),
                text(r"\2\1"),
                text("g"),
            ],
        ),
        (
            "replace_integer_flags_hint",
            "regexp_replace",
            vec![text("abc"), text("b"), text("x"), text("1")],
        ),
        (
            "replace_integer_first_flag_hint",
            "regexp_replace",
            vec![text("abc"), text("b"), text("x"), text("1g")],
        ),
        (
            "replace_trailing_digit_flag",
            "regexp_replace",
            vec![text("abc"), text("b"), text("x"), text("g1")],
        ),
        (
            "replace_negative_n",
            "regexp_replace",
            vec![text("abc"), text("b"), text("x"), int(1), int(-1)],
        ),
    ] {
        assert_reference(&reference, id, name, &arguments, &control);
        assert_eq!(budget.used(), 0, "{id}");
    }
}
