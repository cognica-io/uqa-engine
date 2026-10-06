//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{with_settings, ParserSettings};
use crate::ast::{Expr, Statement};
use uqa_core::Value;

fn legacy() -> ParserSettings {
    ParserSettings {
        standard_conforming_strings: false,
        ..ParserSettings::default()
    }
}

fn literal(statement: &Statement) -> &str {
    let Statement::Select(select) = statement else {
        panic!("SELECT")
    };
    let Expr::Literal(Value::Str(value)) = &select.projections[0].expr else {
        panic!("text literal")
    };
    value
}

#[test]
fn complete_messages_keep_their_original_lexical_settings() {
    let (parsed, metadata) = with_settings(legacy(), || {
        crate::parse_statements(r"SET standard_conforming_strings=on; SELECT 'a\nb'")
    });
    let parsed = parsed.unwrap();
    assert_eq!(metadata.notices.len(), 1);
    let statement = parsed[1].compile().unwrap();
    assert_eq!(literal(&statement), "a\nb");
    assert_eq!(
        literal(&crate::compile(r"SELECT 'a\nb'").unwrap()[0]),
        r"a\nb"
    );
}

#[test]
fn foreign_server_presence_scanning_keeps_original_quote_boundaries() {
    let (parsed, _) = with_settings(legacy(), || {
        crate::parse_statements(
            r"CREATE SERVER s TYPE 'a\'b' VERSION '' FOREIGN DATA WRAPPER wrapper",
        )
    });
    let Statement::CreateForeignServer(server) = parsed.unwrap()[0].compile().unwrap() else {
        panic!("server")
    };
    assert_eq!(server.server_type.as_deref(), Some("a'b"));
    assert_eq!(server.version.as_deref(), Some(""));
}

#[test]
fn deferred_table_text_keeps_already_parsed_string_values() {
    let (statements, _) = with_settings(legacy(), || {
        crate::compile(r"CREATE TABLE IF NOT EXISTS t(v text DEFAULT 'a\nb')")
    });
    let Statement::CreateTableIfNotExists(deferred) = &statements.unwrap()[0] else {
        panic!("deferred table")
    };
    let table = crate::resolve_deferred_create_table(deferred).unwrap();
    assert_eq!(
        table.columns[0].default,
        Some(Expr::Literal(Value::Str("a\nb".into())))
    );
}

#[test]
fn parser_notices_survive_errors_without_message_based_sqlstate_mapping() {
    let (error, metadata) = with_settings(legacy(), || crate::compile(r"SELECT 'a\nb', ("));
    assert_eq!(error.unwrap_err().sqlstate(), Some("42601"));
    assert_eq!(metadata.notices.len(), 1);
    assert_eq!(metadata.notices[0].sqlstate, "22P06");
    let (error, _) = with_settings(legacy(), || crate::compile(r"SELECT U&'d\0061t'"));
    let error = error.unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(error.detail(), Some("String constants with Unicode escapes cannot be used when \"standard_conforming_strings\" is off."));
}

#[test]
fn nested_parser_scopes_restore_after_errors_and_panics() {
    let (result, metadata) = with_settings(legacy(), || {
        let _ = with_settings(ParserSettings::default(), || crate::compile("SELECT ("));
        let panic = std::panic::catch_unwind(|| {
            let _ = with_settings(
                ParserSettings::default(),
                || -> Result<(), crate::SQLError> { panic!("injected compiler panic") },
            );
        });
        assert!(panic.is_err());
        crate::compile(r"SELECT 'a\nb'")
    });
    assert_eq!(literal(&result.unwrap()[0]), "a\nb");
    assert_eq!(metadata.notices.len(), 1);
    assert_eq!(
        literal(&crate::compile(r"SELECT 'a\nb'").unwrap()[0]),
        r"a\nb"
    );
}
