//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn parse_single_label() {
    let e = parse_rpq("knows").unwrap();
    assert_eq!(e, RegularPathExpr::label("knows"));
}
#[test]
fn parse_concat() {
    let e = parse_rpq("knows/likes").unwrap();
    assert_eq!(
        e,
        RegularPathExpr::concat(
            RegularPathExpr::label("knows"),
            RegularPathExpr::label("likes")
        )
    );
}
#[test]
fn parse_alternation_lower_prec_than_concat() {
    let e = parse_rpq("a/b|c").unwrap();
    // a/b first, then alternated with c.
    assert_eq!(
        e,
        RegularPathExpr::alt(
            RegularPathExpr::concat(RegularPathExpr::label("a"), RegularPathExpr::label("b")),
            RegularPathExpr::label("c")
        )
    );
}
#[test]
fn parse_star_binds_tightest() {
    let e = parse_rpq("a*").unwrap();
    assert_eq!(e, RegularPathExpr::star(RegularPathExpr::label("a")));
}
#[test]
fn parse_bounded() {
    let e = parse_rpq("a{2,5}").unwrap();
    assert_eq!(
        e,
        RegularPathExpr::bounded(RegularPathExpr::label("a"), 2, 5)
    );
}
#[test]
fn parse_rejects_reversed_bound() {
    assert!(matches!(
        parse_rpq("a{5,2}"),
        Err(RPQParseError::MalformedBound(message)) if message.contains("exceeds")
    ));
}
#[test]
fn parse_grouping() {
    let e = parse_rpq("(a|b)*").unwrap();
    assert_eq!(
        e,
        RegularPathExpr::star(RegularPathExpr::alt(
            RegularPathExpr::label("a"),
            RegularPathExpr::label("b")
        ))
    );
}

#[test]
fn parse_preserves_unicode_labels_grouping_and_zero_length_bounds() {
    let actual = parse_rpq(" ( 친구 | follows ) / 知る{0,4294967295} ").unwrap();
    assert_eq!(
        actual,
        RegularPathExpr::concat(
            RegularPathExpr::alt(
                RegularPathExpr::label("친구"),
                RegularPathExpr::label("follows")
            ),
            RegularPathExpr::bounded(RegularPathExpr::label("知る"), 0, u32::MAX),
        )
    );
}

#[test]
fn parse_preserves_missing_operand_delimiter_and_bound_diagnostics() {
    for (source, error) in [
        ("", RPQParseError::Eof),
        ("a/", RPQParseError::Eof),
        ("a|", RPQParseError::Eof),
        ("(a", RPQParseError::MissingParen),
        (
            "a)",
            RPQParseError::Unexpected {
                position: 1,
                token: ")".into(),
            },
        ),
        ("a{", RPQParseError::MalformedBound("missing min".into())),
        ("a{1", RPQParseError::MalformedBound("expected ','".into())),
        ("a{1,", RPQParseError::MalformedBound("missing max".into())),
        (
            "a{1,2",
            RPQParseError::MalformedBound("expected '}'".into()),
        ),
    ] {
        assert_eq!(parse_rpq(source).unwrap_err(), error, "{source}");
    }
}
