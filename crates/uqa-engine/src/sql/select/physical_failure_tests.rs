//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::{Batch, ExecError, ExecResult, PhysicalOperator, RowSchema};
use uqa_planner::UnifiedPlan;

struct CloseOperator {
    schema: RowSchema,
    close_error: Option<&'static str>,
}

impl PhysicalOperator for CloseOperator {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        unreachable!("the cleanup helper must not open the operator")
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        unreachable!("the cleanup helper must not pull the operator")
    }

    fn close(&mut self) -> ExecResult<()> {
        match self.close_error {
            Some(message) => Err(ExecError::Other(message.into())),
            None => Ok(()),
        }
    }
}

#[test]
fn physical_failure_preserves_the_original_error_when_close_succeeds() {
    let mut operator = CloseOperator {
        schema: RowSchema::new(Vec::new()),
        close_error: None,
    };
    let original = SQLError::TypeMismatch("primary".into());
    let original_message = original.to_string();
    let error = close_after_physical_failure(&mut operator, ExecError::SQL(original), "execution");
    assert_eq!(error.to_string(), original_message);
    assert!(matches!(error, SQLError::TypeMismatch(_)));
}

#[test]
fn physical_failure_reports_both_execution_and_close_errors() {
    let mut operator = CloseOperator {
        schema: RowSchema::new(Vec::new()),
        close_error: Some("cleanup"),
    };
    let error = close_after_physical_failure(
        &mut operator,
        ExecError::Other("primary".into()),
        "spill buffering",
    );
    let message = error.to_string();
    assert!(message.contains("primary"));
    assert!(message.contains("spill buffering"));
    assert!(message.contains("cleanup"));
}

#[test]
fn floating_limit_rejects_non_finite_fractional_and_out_of_range_values() {
    assert_eq!(float_limit_offset(42.0, "LIMIT").unwrap(), 42);
    for value in [
        f64::NAN,
        f64::INFINITY,
        -1.0,
        1.5,
        18_446_744_073_709_551_616.0,
    ] {
        assert!(float_limit_offset(value, "LIMIT").is_err(), "{value}");
    }
}

#[test]
fn scalar_subquery_scope_restores_the_parent_arena_after_unwind() {
    let statement = uqa_sql::compile("SELECT 1")
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
        panic!("SELECT must lower to a query plan");
    };
    let query = *query;
    let mut scope = CteScope::new();
    scope.scalar_subqueries.push(query.clone());

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = scope.enter_scalar_subqueries(&[query.clone(), query]);
        panic!("exercise scalar-subquery scope cleanup");
    }));

    assert!(unwind.is_err());
    assert_eq!(scope.scalar_subqueries.len(), 1);
}
