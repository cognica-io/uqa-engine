//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{output_values, row_fields, Value};
use uqa_sql::ast::{CreateFunction, Statement};

fn definition(sql: &str) -> CreateFunction {
    let Statement::CreateFunction(definition) = uqa_sql::compile(sql).unwrap().remove(0) else {
        panic!("routine");
    };
    *definition
}

#[test]
fn a_composite_out_parameter_is_one_value_but_a_procedure_returns_its_output_tuple() {
    let function = definition("CREATE FUNCTION f(OUT a record) LANGUAGE sql AS 'SELECT 1'");
    let procedure = definition("CREATE PROCEDURE p(OUT a record) LANGUAGE sql AS 'SELECT 1'");
    let record = Value::Record(vec![
        ("x".into(), Value::Int(1)),
        ("y".into(), Value::Int(2)),
    ]);
    assert_eq!(
        output_values(&function, record.clone(), 1).unwrap(),
        vec![record.clone()]
    );
    let tuple = Value::Record(vec![("a".into(), record.clone())]);
    assert_eq!(output_values(&procedure, tuple, 1).unwrap(), vec![record]);
}

#[test]
fn null_whole_rows_fill_outputs_and_wrong_width_has_postgresql_diagnostic() {
    assert_eq!(
        row_fields(Value::Null, 2).unwrap(),
        vec![Value::Null, Value::Null]
    );
    assert_eq!(
        row_fields(Value::Row(vec![Value::Int(1)].into()), 1).unwrap(),
        vec![Value::Int(1)]
    );
    let error = row_fields(Value::Row(vec![Value::Int(1)].into()), 2).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(
        error.detail(),
        Some("Returned row contains 1 attribute, but query expects 2.")
    );
}
