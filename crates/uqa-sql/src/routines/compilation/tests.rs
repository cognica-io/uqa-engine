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
