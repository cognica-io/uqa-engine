//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Clause comparison checks reuse the types selected by ordered expression validation.

use super::*;
use crate::{ast::FunctionBinding, plan::UnifiedPlan, FunctionTypeResolver};
use std::sync::Mutex;

struct Routines {
    calls: Mutex<Vec<String>>,
    key_type: ColumnType,
}

impl FunctionTypeResolver for Routines {
    fn resolve_function_type(
        &self,
        name: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        match name {
            "key_type" | "bad_type" => self.calls.lock().unwrap().push(name.into()),
            _ => return Ok(None),
        }
        if name == "bad_type" {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "invalid later expression type".into(),
            });
        }
        Ok(Some(self.key_type.clone()))
    }
}

impl RoutineResolution for Routines {}

fn block(sql: &str) -> QueryBlockPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("SELECT")
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = query.root else {
        panic!("query block")
    };
    *block
}

fn validate(sql: &str, routines: &Routines) -> Result<(), SQLError> {
    validate_query_block_expression_types(
        routines,
        &block(sql),
        &RowSchema::with_qualified_types("s", vec!["x".into()], vec![Some(ColumnType::Integer)]),
        &[],
        &crate::binding::fixture::empty_binding_context(),
    )
}

#[test]
fn comparison_clauses_resolve_each_expression_type_once() {
    for (sql, expected) in [
        (
            "SELECT key_type(x) FROM s GROUP BY key_type(x) ORDER BY key_type(x)",
            3,
        ),
        (
            "SELECT key_type(x) FROM s GROUP BY GROUPING SETS ((key_type(x)), ())",
            2,
        ),
        (
            "SELECT DISTINCT ON (key_type(x)) key_type(x) FROM s ORDER BY key_type(x)",
            3,
        ),
    ] {
        let routines = Routines {
            calls: Mutex::new(Vec::new()),
            key_type: ColumnType::Integer,
        };
        validate(sql, &routines).unwrap();
        assert_eq!(routines.calls.lock().unwrap().len(), expected, "{sql}");
    }
}

#[test]
fn later_expression_type_errors_precede_comparison_capability_errors() {
    let routines = Routines {
        calls: Mutex::new(Vec::new()),
        key_type: ColumnType::Json,
    };
    let error = validate(
        "SELECT key_type(x) FROM s GROUP BY key_type(x) HAVING bad_type(x) > 0",
        &routines,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(error.to_string(), "invalid later expression type");
    assert_eq!(
        *routines.calls.lock().unwrap(),
        ["key_type", "key_type", "bad_type"]
    );
    routines.calls.lock().unwrap().clear();
    let error = validate("SELECT key_type(x) FROM s GROUP BY key_type(x)", &routines).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(
        error.to_string(),
        "could not identify an equality operator for type json"
    );
}

#[test]
fn clause_type_reuse_preserves_postgresql_error_order_and_operator_diagnostics() {
    // Independently captured from Docker PostgreSQL 18.4, including HAVING type validation before GROUP BY capability validation.
    for (sql, message) in [
        (
            "SELECT '{}'::json GROUP BY '{}'::json HAVING sqrt(true)>0",
            "function sqrt(boolean) does not exist",
        ),
        (
            "SELECT '{}'::json GROUP BY '{}'::json",
            "could not identify an equality operator for type json",
        ),
        (
            "SELECT '{}'::json ORDER BY '{}'::json",
            "could not identify an ordering operator for type json",
        ),
    ] {
        let routines = Routines {
            calls: Mutex::new(Vec::new()),
            key_type: ColumnType::Integer,
        };
        let error = validate(sql, &routines).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
        assert_eq!(error.to_string(), message, "{sql}");
    }
}
