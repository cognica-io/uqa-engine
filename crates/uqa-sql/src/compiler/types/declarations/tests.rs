//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{AlterTableAction, FunctionBinding, Statement},
    type_resolution::{resolve_alter_column_type, FunctionTypeResolver},
};
use std::sync::Mutex;

fn action(sql: &str) -> AlterTableAction {
    let Some(Statement::AlterTable(mut table)) = crate::compiler::compile(sql).unwrap().pop()
    else {
        panic!("expected ALTER TABLE");
    };
    table.actions.pop().unwrap()
}

fn declared_type(action: &AlterTableAction) -> &ColumnType {
    match action {
        AlterTableAction::AddColumn { column, .. } => &column.ty,
        AlterTableAction::AlterColumnType { ty, .. } => ty,
        _ => panic!("expected a type declaration"),
    }
}

#[derive(Default)]
struct Types {
    names: Mutex<Vec<String>>,
}

impl FunctionTypeResolver for Types {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        self.names.lock().unwrap().push(name.to_owned());
        Ok(Some(ColumnType::Text))
    }

    fn resolve_function_type(
        &self,
        _name: &str,
        _binding: Option<&FunctionBinding>,
        _argument_names: &[Option<String>],
        _argument_types: &[Option<ColumnType>],
        _explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

#[test]
fn alter_preserves_invalid_modifiers_until_the_target_admits_the_declaration() {
    let types = Types::default();
    for (declaration, state, message) in [
        (
            "numeric(0)",
            "22023",
            "NUMERIC precision 0 must be between 1 and 1000",
        ),
        (
            "numeric(2,-1001)[]",
            "22023",
            "NUMERIC scale -1001 must be between -1000 and 1000",
        ),
        (
            "varchar(0)",
            "22023",
            "length for type varchar must be at least 1",
        ),
    ] {
        for sql in [
            format!("ALTER TABLE IF EXISTS absent ADD COLUMN IF NOT EXISTS a {declaration}"),
            format!("ALTER TABLE t ALTER COLUMN a TYPE {declaration} USING missing"),
        ] {
            let action = action(&sql);
            let ty = declared_type(&action);
            assert!(matches!(ty, ColumnType::Named(_)), "{sql}");
            let error = resolve_alter_column_type(&types, ty).unwrap_err();
            assert_eq!(error.sqlstate(), Some(state), "{sql}");
            assert_eq!(error.to_string(), message, "{sql}");
        }
    }
    assert!(types.names.lock().unwrap().is_empty());
}

#[test]
fn alter_retains_noninteger_modifier_syntax_without_analyzing_it() {
    for (declaration, expression) in [
        ("numeric(bad_precision)", "bad_precision"),
        ("numeric(1+1)", "1 + 1"),
        ("numeric($1)", "$1"),
    ] {
        for clause in ["ADD COLUMN a", "ALTER COLUMN a TYPE"] {
            let action = action(&format!("ALTER TABLE t {clause} {declaration}"));
            let ColumnType::Named(retained) = declared_type(&action) else {
                panic!("modifier must be retained");
            };
            assert!(retained.contains(expression), "{retained}");
            assert!(parse_regtype_name(retained).unwrap().is_some());
        }
    }
}

#[test]
fn alter_replays_fallible_declarations_without_modifiers_at_type_resolution() {
    for declaration in ["void[]", "vector"] {
        let expected =
            crate::compiler::compile(&format!("CREATE TABLE t (a {declaration})")).unwrap_err();
        let action = action(&format!(
            "ALTER TABLE IF EXISTS absent ADD COLUMN a {declaration}"
        ));
        let error =
            resolve_alter_column_type(&Types::default(), declared_type(&action)).unwrap_err();
        assert_eq!(error.sqlstate(), expected.sqlstate(), "{declaration}");
        assert_eq!(error.to_string(), expected.to_string(), "{declaration}");
    }
}

#[test]
fn alter_keeps_valid_modifiers_and_named_type_identity() {
    let types = Types::default();
    for (declaration, expected) in [
        (
            "numeric(6,2)[]",
            ColumnType::Array(Box::new(ColumnType::Numeric {
                precision: Some(6),
                scale: Some(2),
            })),
        ),
        ("varchar(12)", ColumnType::Varchar(Some(12))),
        (
            "interval day to second(3)",
            ColumnType::IntervalWithFields {
                fields: crate::ast::IntervalFields::DayToSecond,
                precision: Some(3),
            },
        ),
    ] {
        let action = action(&format!("ALTER TABLE t ADD COLUMN a {declaration}"));
        assert_eq!(
            resolve_alter_column_type(&types, declared_type(&action)).unwrap(),
            expected
        );
    }
    for (declaration, expected) in [
        ("types.numeric(0)", "\"types\".\"numeric\"(0)"),
        ("\"integer\"(0)", "\"integer\"(0)"),
        ("\"Case\".\"Numeric\"(4)", "\"Case\".\"Numeric\"(4)"),
    ] {
        let action = action(&format!("ALTER TABLE t ADD COLUMN a {declaration}"));
        assert_eq!(
            resolve_alter_column_type(&types, declared_type(&action)).unwrap(),
            ColumnType::Text
        );
        assert_eq!(types.names.lock().unwrap().last().unwrap(), expected);
    }
}

#[test]
fn alter_preserves_serial_and_existing_modifier_free_types() {
    for (declaration, expected, serial_array) in [
        ("integer", ColumnType::Integer, false),
        ("serial", ColumnType::Integer, false),
        (
            "serial[]",
            ColumnType::Array(Box::new(ColumnType::Integer)),
            true,
        ),
    ] {
        let AlterTableAction::AddColumn {
            column,
            declaration: syntax,
            ..
        } = action(&format!("ALTER TABLE t ADD COLUMN a {declaration}"))
        else {
            panic!("expected ADD COLUMN");
        };
        assert_eq!(column.ty, expected);
        assert_eq!(syntax.serial_array, serial_array);
        assert_eq!(
            column.auto_increment.is_some(),
            declaration.starts_with("serial")
        );
    }
}
