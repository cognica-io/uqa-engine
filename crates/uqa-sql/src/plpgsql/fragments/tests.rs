//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
use super::*;
use crate::{
    parser::{with_settings, ParserSettings},
    plpgsql::{
        lowering_expression::{lower_expr, lower_full_statement},
        parsing::parse_plpgsql_text,
        PLpgSQLCompileMode,
    },
};

fn expression(query: &str, mode: PLpgSQLCompileMode) -> PLpgSQLExpression {
    lower_expr(
        &serde_json::json!({"PLpgSQL_expr":{"query":query,"parseMode":2}}),
        mode,
    )
    .unwrap()
}
#[test]
fn equal_source_text_has_distinct_occurrences_but_clones_share_identity() {
    let first = expression("'a\\nb'", PLpgSQLCompileMode::Validate);
    let second = expression("'a\\nb'", PLpgSQLCompileMode::Validate);
    assert_eq!(first.source().query, second.source().query);
    assert_ne!(first.site(), second.site());
    assert_eq!(first.site(), first.clone().site());
}
#[test]
fn runtime_metadata_defers_syntax_and_uses_first_reached_parser_settings() {
    let raw = serde_json::json!({"PLpgSQL_expr":{"query":"SELECT 1 +"}});
    let statement = lower_full_statement(&raw, PLpgSQLCompileMode::Runtime).unwrap();
    assert!(statement.validation().is_none());
    assert!(statement.parse().is_err());
    assert!(lower_full_statement(&raw, PLpgSQLCompileMode::Validate).is_err());
    let expr = expression(r"'a\nb'", PLpgSQLCompileMode::Runtime);
    let off = ParserSettings {
        standard_conforming_strings: false,
        ..ParserSettings::default()
    };
    let (parsed, notices) = with_settings(off, || expr.parse());
    assert!(matches!(parsed.unwrap(), Expr::Literal(uqa_core::Value::Str(v)) if v == "a\nb"));
    assert_eq!(notices.notices.len(), 1);
    let (parsed, notices) = with_settings(ParserSettings::default(), || expr.parse());
    assert!(matches!(parsed.unwrap(), Expr::Literal(uqa_core::Value::Str(v)) if v == r"a\nb"));
    assert!(notices.notices.is_empty());
}
#[test]
fn declaration_validation_does_not_prepare_occurrences_or_duplicate_parser_notices() {
    let (parsed, notices) = with_settings(
        ParserSettings {
            standard_conforming_strings: false,
            ..ParserSettings::default()
        },
        || {
            parse_plpgsql_text(
                r"CREATE FUNCTION f() RETURNS text LANGUAGE plpgsql AS $$BEGIN RETURN 'a\nb'; END$$",
            )
        },
    );
    assert!(parsed.is_ok());
    assert_eq!(notices.notices.len(), 2);
}
#[test]
fn initializer_ownership_preserves_nested_blocks_and_cursor_arguments() {
    let parsed = parse_plpgsql_text("CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$DECLARE x int:=1; c CURSOR(a int) FOR SELECT a; BEGIN IF false THEN DECLARE x text:='nested'; BEGIN NULL; END; END IF; END$$").unwrap();
    let all = parsed.block_variable_datums();
    let cursor = parsed.cursor_argument_datums();
    assert!(!cursor.is_empty());
    // PostgreSQL initializes cursor arguments with their declaring block even
    // though their names belong only to the cursor query namespace.
    assert!(cursor.is_subset(&all));
    assert!(cursor
        .iter()
        .all(|index| parsed.action.initvarnos.contains(index)));
    let x: Vec<_> = parsed
        .datums
        .iter()
        .enumerate()
        .filter(|(_, d)| d.name() == Some("x"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(x.len(), 2);
    assert!(x.iter().all(|i| all.contains(i)));
    assert!(parsed.action.initvarnos.contains(&x[0]));
    assert!(!parsed.action.initvarnos.contains(&x[1]));
}
