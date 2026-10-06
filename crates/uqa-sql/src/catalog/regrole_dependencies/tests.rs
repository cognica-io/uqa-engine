//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;

#[derive(Default)]
struct Roles {
    reads: Cell<usize>,
}

impl StoredRegroleResolver for Roles {
    fn resolve_stored_regrole(&self, input: &str) -> Result<Option<i64>, SQLError> {
        self.reads.set(self.reads.get() + 1);
        input.parse().map(Some).map_err(|_| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("role \"{input}\" does not exist"),
        })
    }
}

fn expression(sql: &str) -> Expr {
    let crate::Statement::Select(mut statement) =
        crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        unreachable!()
    };
    statement.projections.remove(0).expr
}

#[test]
fn converted_role_constants_keep_dependencies_without_rereading() {
    for oid in [0, 23] {
        let converted = Expr::TypedLiteral {
            value: Value::Int(oid),
            ty: "regrole".into(),
        };
        for stored in [
            converted.clone(),
            Expr::Cast {
                expr: Box::new(converted),
                ty: "oid".into(),
                implicit: true,
            },
        ] {
            let roles = Roles::default();
            let mut constants = StoredRegroleConstants::default();
            constants.collect_expression(&stored, None);
            let error = constants.reject_with(&roles).unwrap_err();
            assert_eq!(error.sqlstate(), Some("0A000"));
            assert_eq!(
                error.to_string(),
                "constant of the type regrole cannot be used here"
            );
            assert_eq!(roles.reads.get(), 0, "bound inputs are not read again");
        }
    }
}

#[test]
fn raw_input_errors_precede_dependency_rejection() {
    let roles = Roles::default();
    let mut constants = StoredRegroleConstants::default();
    constants.collect_expression(
        &Expr::Literal(Value::Str("missing".into())),
        Some(&ColumnType::Regrole),
    );
    let error = constants.reject_with(&roles).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert_eq!(roles.reads.get(), 1);
}

#[test]
fn runtime_numeric_null_and_array_role_expressions_remain_storable() {
    let roles = Roles::default();
    for sql in [
        "('23'::text)::regrole",
        "10::regrole",
        "NULL::regrole",
        "'{23}'::regrole[]",
    ] {
        let stored = expression(sql);
        let mut constants = StoredRegroleConstants::default();
        constants.collect_expression(&stored, None);
        constants
            .reject_with(&roles)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn query_dependency_collection_distinguishes_typed_null_and_role_constants() {
    for (value, rejected) in [(Value::Int(23), true), (Value::Null, false)] {
        let crate::Statement::Select(mut query) = crate::compile("SELECT 23").unwrap().remove(0)
        else {
            unreachable!()
        };
        query.projections[0].expr = Expr::TypedLiteral {
            value,
            ty: "regrole".into(),
        };
        let mut plan = QueryPlan::lower(*query);
        let mut constants = StoredRegroleConstants::default();
        constants.collect_query_plan(&mut plan);
        assert_eq!(constants.reject_with(&Roles::default()).is_err(), rejected);
    }
}
