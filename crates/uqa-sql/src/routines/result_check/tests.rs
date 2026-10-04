//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::check_sql_function_result;
use crate::{
    ast::{ColumnDef, ColumnType, CreateFunction, Statement},
    binding::statements::AnalyzedResult,
    routines::declaration::RoutineTypeCatalog,
    SQLError,
};

struct Types;

impl RoutineTypeCatalog for Types {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(None)
    }

    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        ColumnType::from_sql_name(name).ok()
    }

    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        ColumnType::from_sql_name(name)
    }

    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }

    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }

    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.regtype_name())
    }
}

fn definition(header: &str) -> CreateFunction {
    let Statement::CreateFunction(definition) =
        crate::compile(&format!("CREATE {header} LANGUAGE sql AS 'SELECT 1'"))
            .unwrap()
            .remove(0)
    else {
        panic!("expected a routine definition");
    };
    *definition
}

/// The message and detail of the check, or `None` when it passes.
fn checked(header: &str, last: Option<AnalyzedResult>) -> Option<(String, String)> {
    match check_sql_function_result(&Types, &definition(header), last.as_ref()) {
        Ok(()) => None,
        Err(SQLError::Diagnostic {
            sqlstate,
            message,
            detail,
            ..
        }) => {
            assert_eq!(sqlstate, "42P13");
            Some((message, detail.unwrap()))
        }
        Err(other) => panic!("unexpected error {other:?}"),
    }
}

fn rows(types: &[Option<ColumnType>]) -> Option<AnalyzedResult> {
    Some(AnalyzedResult::Rows(types.to_vec()))
}

const INTEGER: &str = "return type mismatch in function declared to return integer";
const RECORD: &str = "return type mismatch in function declared to return record";

#[test]
fn a_void_function_may_end_with_any_statement() {
    assert_eq!(checked("FUNCTION f() RETURNS void", None), None);
    assert_eq!(
        checked("FUNCTION f() RETURNS void", Some(AnalyzedResult::Command)),
        None
    );
    assert_eq!(checked("PROCEDURE p()", None), None);
}

#[test]
fn a_scalar_result_needs_one_assignable_column() {
    let final_statement = |detail: &str| Some((INTEGER.to_string(), detail.to_string()));
    let must_return =
        "Function's final statement must be SELECT or INSERT/UPDATE/DELETE/MERGE RETURNING.";
    assert_eq!(
        checked("FUNCTION f() RETURNS int", None),
        final_statement(must_return)
    );
    assert_eq!(
        checked("FUNCTION f() RETURNS int", Some(AnalyzedResult::Command)),
        final_statement(must_return)
    );
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS int",
            rows(&[Some(ColumnType::Integer), Some(ColumnType::Integer)])
        ),
        final_statement("Final statement must return exactly one column.")
    );
    assert_eq!(
        checked("FUNCTION f() RETURNS int", rows(&[Some(ColumnType::Text)])),
        final_statement("Actual return type is text.")
    );
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS boolean",
            rows(&[Some(ColumnType::Integer)])
        ),
        Some((
            "return type mismatch in function declared to return boolean".into(),
            "Actual return type is integer.".into()
        ))
    );
    for source in [
        ColumnType::BigInteger,
        ColumnType::Numeric {
            precision: None,
            scale: None,
        },
        ColumnType::Integer,
    ] {
        assert_eq!(
            checked("FUNCTION f() RETURNS int", rows(&[Some(source)])),
            None
        );
    }
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS text",
            rows(&[Some(ColumnType::Integer)])
        ),
        None
    );
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS varchar(3)",
            rows(&[Some(ColumnType::Text)])
        ),
        None
    );
    // A single output column makes the function return that column's type.
    assert_eq!(
        checked(
            "FUNCTION f() RETURNS TABLE(a int)",
            rows(&[Some(ColumnType::Text)])
        ),
        final_statement("Actual return type is text.")
    );
}

#[test]
fn a_row_result_checks_each_column_in_order() {
    let table = "FUNCTION f() RETURNS TABLE(a int, b int)";
    let detail = |detail: &str| Some((RECORD.to_string(), detail.to_string()));
    assert_eq!(
        checked(table, rows(&[Some(ColumnType::Integer)])),
        detail("Final statement returns too few columns.")
    );
    assert_eq!(
        checked(
            table,
            rows(&[
                Some(ColumnType::Integer),
                Some(ColumnType::Integer),
                Some(ColumnType::Integer)
            ])
        ),
        detail("Final statement returns too many columns.")
    );
    assert_eq!(
        checked(
            table,
            rows(&[
                Some(ColumnType::Text),
                Some(ColumnType::Integer),
                Some(ColumnType::Integer)
            ])
        ),
        detail("Final statement returns text instead of integer at column 1.")
    );
    assert_eq!(
        checked(
            table,
            rows(&[Some(ColumnType::Integer), Some(ColumnType::Text)])
        ),
        detail("Final statement returns text instead of integer at column 2.")
    );
    assert_eq!(
        checked(
            "FUNCTION f(OUT a int, OUT b text)",
            rows(&[Some(ColumnType::Integer)])
        ),
        detail("Final statement returns too few columns.")
    );
    assert_eq!(
        checked(table, rows(&[Some(ColumnType::Integer), None])),
        None
    );
    // A function returns a lone row column as its whole result; a procedure assigns its output parameters column by column.
    assert_eq!(checked(table, rows(&[Some(ColumnType::Record)])), None);
    assert_eq!(
        checked("PROCEDURE p(OUT a int)", rows(&[Some(ColumnType::Record)])),
        detail("Final statement returns record instead of integer at column 1.")
    );
    assert_eq!(
        checked("PROCEDURE p(OUT a int)", rows(&[Some(ColumnType::Integer)])),
        None
    );
}

#[test]
fn a_record_without_output_parameters_accepts_any_row() {
    for header in [
        "FUNCTION f() RETURNS record",
        "FUNCTION f() RETURNS SETOF record",
    ] {
        assert_eq!(
            checked(
                header,
                rows(&[Some(ColumnType::Integer), Some(ColumnType::Text)])
            ),
            None
        );
        assert_eq!(
            checked(header, Some(AnalyzedResult::Command)),
            Some((
                RECORD.to_string(),
                "Function's final statement must be SELECT or INSERT/UPDATE/DELETE/MERGE RETURNING."
                    .to_string()
            ))
        );
    }
}
