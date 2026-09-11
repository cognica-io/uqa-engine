//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Batch, ExecError, ExecResult, PhysicalOperator, RowSchema};

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
