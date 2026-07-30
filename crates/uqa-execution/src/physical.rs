//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volcano `PhysicalOperator` trait shared by every operator in this
//! crate. Mirrors UQA `execution/physical`.

use thiserror::Error;

use crate::batch::Batch;

/// Operator-pipeline error type. Wraps SQL evaluation errors so call
/// sites do not need to juggle two error enums.
#[derive(Debug, Error)]
pub enum ExecError {
    #[error("execution error: {0}")]
    Other(String),
    #[error("SQL error: {0}")]
    SQL(#[from] uqa_sql::SQLError),
}

pub type ExecResult<T> = std::result::Result<T, ExecError>;

/// Preserve the primary execution result while still surfacing a cleanup
/// failure. `Result::and` discards the cleanup error whenever both operations
/// fail, which makes file/child-close failures disappear behind the original
/// operator error.
pub(crate) fn with_cleanup<T>(
    primary: ExecResult<T>,
    cleanup: ExecResult<()>,
    cleanup_context: &str,
) -> ExecResult<T> {
    match (primary, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(primary_error), Ok(())) => Err(primary_error),
        (Err(primary_error), Err(cleanup_error)) => Err(ExecError::Other(format!(
            "{primary_error}; {cleanup_context}: {cleanup_error}"
        ))),
    }
}

/// Volcano-style streaming operator. Operators form a tree; each
/// operator pulls from its children inside `next` and emits a
/// [`Batch`] until the input is exhausted.
///
/// Pipeline lifecycle:
///
/// 1. `open` -- bind state, open child operators, allocate buffers.
/// 2. `next` -- pull the next [`Batch`]; return `None` to terminate.
/// 3. `close` -- release buffers and close child operators.
///
/// Blocking operators (sort / hash-aggregate) materialise their input
/// during `open`; pipelined operators (filter / project / limit) emit
/// batches as they arrive.
pub trait PhysicalOperator: Send {
    /// Schema (column names, in order) the operator will emit.
    fn schema(&self) -> &[String];

    fn open(&mut self) -> ExecResult<()>;
    fn next(&mut self) -> ExecResult<Option<Batch>>;
    fn close(&mut self) -> ExecResult<()>;
}

/// Convenience: collect every batch from `op` until exhaustion. The
/// operator is `open`ed and `close`d for the caller. Useful for tests
/// and for callers that do not need streaming behaviour.
pub fn run_to_batches(op: &mut dyn PhysicalOperator) -> ExecResult<Vec<Batch>> {
    if let Err(open_error) = op.open() {
        return match op.close() {
            Ok(()) => Err(open_error),
            Err(close_error) => Err(ExecError::Other(format!(
                "{open_error}; operator close after open failure also failed: {close_error}"
            ))),
        };
    }
    let mut out = Vec::new();
    loop {
        match op.next() {
            Ok(Some(batch)) => out.push(batch),
            Ok(None) => break,
            Err(next_error) => {
                return match op.close() {
                    Ok(()) => Err(next_error),
                    Err(close_error) => Err(ExecError::Other(format!(
                        "{next_error}; operator close after execution failure also failed: {close_error}"
                    ))),
                };
            }
        }
    }
    op.close()?;
    Ok(out)
}

/// Run the operator and concatenate all output batches into a single
/// flat row vector. The schema of the first batch is the result schema.
pub fn run_to_rows(
    op: &mut dyn PhysicalOperator,
) -> ExecResult<(Vec<String>, Vec<uqa_sql::ResultRow>)> {
    let schema = op.schema().to_vec();
    let batches = run_to_batches(op)?;
    let mut rows: Vec<uqa_sql::ResultRow> = Vec::new();
    for batch in batches {
        rows.extend(batch.rows);
    }
    Ok((schema, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingOperator {
        fail_open: bool,
        fail_close: bool,
        closed: bool,
    }

    impl PhysicalOperator for FailingOperator {
        fn schema(&self) -> &[String] {
            &[]
        }

        fn open(&mut self) -> ExecResult<()> {
            if self.fail_open {
                Err(ExecError::Other("open failed".into()))
            } else {
                Ok(())
            }
        }

        fn next(&mut self) -> ExecResult<Option<Batch>> {
            Err(ExecError::Other("next failed".into()))
        }

        fn close(&mut self) -> ExecResult<()> {
            self.closed = true;
            if self.fail_close {
                Err(ExecError::Other("close failed".into()))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn runner_closes_after_open_and_next_failures() {
        let mut open = FailingOperator {
            fail_open: true,
            fail_close: false,
            closed: false,
        };
        assert!(run_to_batches(&mut open)
            .unwrap_err()
            .to_string()
            .contains("open failed"));
        assert!(open.closed);

        let mut next = FailingOperator {
            fail_open: false,
            fail_close: false,
            closed: false,
        };
        assert!(run_to_batches(&mut next)
            .unwrap_err()
            .to_string()
            .contains("next failed"));
        assert!(next.closed);
    }

    #[test]
    fn runner_reports_execution_and_cleanup_failures() {
        let mut operator = FailingOperator {
            fail_open: false,
            fail_close: true,
            closed: false,
        };
        let error = run_to_batches(&mut operator).unwrap_err().to_string();
        assert!(error.contains("next failed"), "{error}");
        assert!(error.contains("close failed"), "{error}");
        assert!(operator.closed);
    }

    #[test]
    fn cleanup_combiner_preserves_both_errors() {
        let error = with_cleanup::<()>(
            Err(ExecError::Other("primary".into())),
            Err(ExecError::Other("cleanup".into())),
            "cleanup failed",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("primary"), "{error}");
        assert!(error.contains("cleanup"), "{error}");
    }
}
