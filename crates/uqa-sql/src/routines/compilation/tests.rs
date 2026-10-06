//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn sql_standard_foreign_table_utilities_fail_before_dependency_binding() {
    for clause in ["", "IF NOT EXISTS "] {
        let Statement::CreateFunction(definition) = crate::compile(&format!(
            "CREATE FUNCTION make_foreign() RETURNS void LANGUAGE sql BEGIN ATOMIC CREATE FOREIGN TABLE {clause}created(a integer) SERVER missing_server; END"
        ))
        .unwrap()
        .remove(0) else {
            panic!("expected routine definition");
        };
        let FunctionBody::Statements(statements) = definition.body else {
            panic!("expected SQL-standard body");
        };
        let error = validate_sql_standard_statement(&statements[0]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"));
        assert_eq!(
            error.to_string(),
            "CREATE FOREIGN TABLE is not yet supported in unquoted SQL function body"
        );
    }
}

#[test]
fn sql_standard_drop_server_fails_before_target_resolution() {
    for declaration in [
        "FUNCTION remove_server() RETURNS void",
        "PROCEDURE remove_server()",
    ] {
        for command in [
            "DROP SERVER missing_server",
            "DROP SERVER IF EXISTS missing_server CASCADE",
            "DROP SERVER first_server, other_server RESTRICT",
        ] {
            let Statement::CreateFunction(definition) = crate::compile(&format!(
                "CREATE {declaration} LANGUAGE SQL BEGIN ATOMIC {command}; END"
            ))
            .unwrap()
            .remove(0) else {
                panic!("expected routine definition");
            };
            let FunctionBody::Statements(statements) = definition.body else {
                panic!("expected SQL-standard body");
            };
            let error = validate_sql_standard_statement(&statements[0]).unwrap_err();
            assert_eq!(error.sqlstate(), Some("0A000"));
            assert_eq!(
                error.to_string(),
                "DROP SERVER is not yet supported in unquoted SQL function body"
            );
        }
    }
}

#[test]
fn quoted_drop_server_body_keeps_execution_time_compilation() {
    let Statement::CreateFunction(definition) = crate::compile(
        "CREATE FUNCTION remove_server() RETURNS void LANGUAGE SQL AS 'DROP SERVER IF EXISTS missing_server'",
    )
    .unwrap()
    .remove(0) else {
        panic!("expected routine definition");
    };
    let FunctionBody::Source(source) = definition.body else {
        panic!("expected quoted source body");
    };
    let Statement::Drop(drop) = crate::compile(&source).unwrap().remove(0) else {
        panic!("expected execution-time DROP SERVER");
    };
    assert_eq!(drop.kind, crate::ast::DropKind::ForeignServer);
    assert!(drop.if_exists);
}

#[test]
fn polymorphic_result_errors_keep_postgresql_message_and_detail_separate() {
    for (result, inputs) in [
        ("anyelement", "anyelement, anyarray, anynonarray, anyenum, anyrange, or anymultirange"),
        ("anycompatible", "anycompatible, anycompatiblearray, anycompatiblenonarray, anycompatiblerange, or anycompatiblemultirange"),
    ] {
        let Statement::CreateFunction(definition) = crate::compile(&format!(
            "CREATE FUNCTION result_type(a integer) RETURNS {result} LANGUAGE SQL AS 'SELECT 1'"
        )).unwrap().remove(0) else { unreachable!() };
        let error = validate_routine_declaration(&definition).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P13"));
        assert_eq!(error.to_string(), "cannot determine result data type");
        assert!(matches!(error, SQLError::Diagnostic { detail: Some(detail), hint: None, .. }
            if detail == format!("A result of type {result} requires at least one input of type {inputs}.")));
    }
}
