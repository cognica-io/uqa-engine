//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::ArrayValue;

const DEFAULT_COLLATION: u32 = uqa_sql::catalog::collations::DEFAULT_COLLATION_OID as u32;

fn text(value: &str) -> Value {
    Value::Str(value.into())
}

fn captures(items: &[Option<&str>]) -> crate::PhysicalRow {
    crate::PhysicalRow::from_values(vec![Value::Array(
        ArrayValue::try_new(
            items
                .iter()
                .map(|item| item.map_or(Value::Null, text))
                .collect(),
        )
        .unwrap(),
    )])
}

#[test]
fn regexp_matches_streams_one_row_per_match_and_none_for_null_inputs() {
    for (arguments, expected) in [
        (
            vec![text("aab"), text("a*"), text("g")],
            vec![
                captures(&[Some("aa")]),
                captures(&[Some("")]),
                captures(&[Some("")]),
            ],
        ),
        (
            vec![text("ab"), text("(a)|(b)"), text("g")],
            vec![captures(&[Some("a"), None]), captures(&[None, Some("b")])],
        ),
        (
            vec![text("a1b2"), text("[0-9]")],
            vec![captures(&[Some("1")])],
        ),
        (vec![text("abc"), text("z"), text("g")], Vec::new()),
        (vec![Value::Null, text("a")], Vec::new()),
        (vec![text("a"), Value::Null], Vec::new()),
        (vec![text("a"), text("a"), Value::Null], Vec::new()),
    ] {
        let output = regexp_matches_row_stream(&arguments, DEFAULT_COLLATION, "m".into()).unwrap();
        assert_eq!(output.columns, ["m"]);
        let rows = output.rows.collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(rows, expected, "{arguments:?}");
    }
}

#[test]
fn regexp_matches_reports_flag_errors_before_the_first_row() {
    for (flags, message) in [
        ("z", "invalid regular expression option: \"z\""),
        ("gz", "invalid regular expression option: \"z\""),
    ] {
        let Err(error) = regexp_matches_row_stream(
            &[text("abc"), text("b"), text(flags)],
            DEFAULT_COLLATION,
            "m".into(),
        ) else {
            panic!("flags {flags} must be rejected");
        };
        assert_eq!(error.sqlstate(), Some("22023"));
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn regexp_split_to_table_streams_pieces_and_none_for_null_inputs() {
    let piece = |text_value: &str| crate::PhysicalRow::from_values(vec![text(text_value)]);
    for (arguments, expected) in [
        (
            vec![text(",a,"), text(",")],
            vec![piece(""), piece("a"), piece("")],
        ),
        (
            vec![text("baaac"), text("a*")],
            vec![piece("b"), piece("c")],
        ),
        (
            vec![text("aXbxc"), text("x"), text("i")],
            vec![piece("a"), piece("b"), piece("c")],
        ),
        (vec![text(""), text("x")], vec![piece("")]),
        (vec![Value::Null, text(",")], Vec::new()),
        (vec![text("a"), text(","), Value::Null], Vec::new()),
    ] {
        let output =
            regexp_split_to_table_row_stream(&arguments, DEFAULT_COLLATION, "p".into()).unwrap();
        assert_eq!(output.columns, ["p"]);
        let rows = output.rows.collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(rows, expected, "{arguments:?}");
    }
    let Err(error) = regexp_split_to_table_row_stream(
        &[text("abc"), text("b"), text("g")],
        DEFAULT_COLLATION,
        "p".into(),
    ) else {
        panic!("g must be rejected");
    };
    assert_eq!(
        error.to_string(),
        "regexp_split_to_table() does not support the \"global\" option"
    );
}
