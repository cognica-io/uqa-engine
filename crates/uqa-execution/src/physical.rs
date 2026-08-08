//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volcano `PhysicalOperator` trait shared by every operator in this crate.

use thiserror::Error;

use crate::batch::{Batch, RowSchema};

/// A leading physical row-order property carried between operators.
///
/// Column names refer to the operator's output schema. `nulls_first` is
/// irrelevant when `nullable` is false, which lets a primary-key scan satisfy
/// either explicit NULLS placement without adding a redundant sort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalOrder {
    pub column: String,
    pub descending: bool,
    pub nulls_first: Option<bool>,
    pub nullable: bool,
}

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
    /// Complete logical-to-physical row layout emitted by this operator.
    fn row_schema(&self) -> &RowSchema;

    /// Schema column names in logical output order.
    fn schema(&self) -> &[String] {
        self.row_schema().columns()
    }

    /// Planner/runtime cardinality estimate for choosing physical strategies.
    /// `None` means the operator cannot provide a useful estimate. The value
    /// is advisory rather than a correctness bound.
    fn estimated_cardinality(&self) -> Option<u64> {
        None
    }

    /// Leading output ordering known to be preserved by this operator.
    fn output_ordering(&self) -> &[PhysicalOrder] {
        &[]
    }

    /// Let a leaf consume its native projected rows directly into an
    /// aggregate executor. Returning `false` promises that no input was
    /// consumed, so the caller can fall back to ordinary `Batch` pulls.
    fn consume_into_aggregate(
        &mut self,
        _executor: &mut dyn crate::relational::AggregateExecutor,
    ) -> ExecResult<bool> {
        Ok(false)
    }

    fn open(&mut self) -> ExecResult<()>;
    fn next(&mut self) -> ExecResult<Option<Batch>>;
    fn close(&mut self) -> ExecResult<()>;
}

/// Return whether `actual` has `required` as an ordering prefix.
pub fn ordering_satisfies(actual: &[PhysicalOrder], required: &[PhysicalOrder]) -> bool {
    actual.len() >= required.len()
        && actual.iter().zip(required).all(|(actual, required)| {
            actual.column == required.column
                && actual.descending == required.descending
                && (!actual.nullable
                    || actual.nulls_first == required.nulls_first
                    || required.nulls_first.is_none())
        })
}

/// Borrowing iterator over a physical operator. Construction opens the
/// pipeline; exhaustion and execution errors close it exactly once. Dropping
/// an unfinished cursor performs best-effort cleanup.
pub struct OperatorBatchCursor<'operator> {
    operator: &'operator mut dyn PhysicalOperator,
    finished: bool,
}

impl<'operator> OperatorBatchCursor<'operator> {
    pub fn open(operator: &'operator mut dyn PhysicalOperator) -> ExecResult<Self> {
        if let Err(open_error) = operator.open() {
            return match operator.close() {
                Ok(()) => Err(open_error),
                Err(close_error) => Err(ExecError::Other(format!(
                    "{open_error}; operator close after open failure also failed: {close_error}"
                ))),
            };
        }
        Ok(Self {
            operator,
            finished: false,
        })
    }

    fn finish(&mut self) -> ExecResult<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.operator.close()
    }
}

impl Iterator for OperatorBatchCursor<'_> {
    type Item = ExecResult<Batch>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match self.operator.next() {
            Ok(Some(batch)) => Some(Ok(batch)),
            Ok(None) => match self.finish() {
                Ok(()) => None,
                Err(error) => Some(Err(error)),
            },
            Err(next_error) => {
                let close = self.finish();
                Some(with_cleanup(
                    Err(next_error),
                    close,
                    "operator close after execution failure also failed",
                ))
            }
        }
    }
}

impl Drop for OperatorBatchCursor<'_> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// Convenience: collect every batch from `op` until exhaustion. The
/// operator is `open`ed and `close`d for the caller. Useful for tests
/// and for callers that do not need streaming behaviour.
pub fn run_to_batches(op: &mut dyn PhysicalOperator) -> ExecResult<Vec<Batch>> {
    OperatorBatchCursor::open(op)?.collect()
}

/// Run the operator and concatenate all output batches into a single
/// flat row vector. The schema of the first batch is the result schema.
pub fn run_to_rows(
    op: &mut dyn PhysicalOperator,
) -> ExecResult<(Vec<String>, Vec<uqa_sql::ResultRow>)> {
    let schema = op.schema().to_vec();
    let mut rows: Vec<uqa_sql::ResultRow> = Vec::new();
    for batch in OperatorBatchCursor::open(op)? {
        let batch = batch?;
        rows.extend(batch.into_result_rows());
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
        fn row_schema(&self) -> &RowSchema {
            static SCHEMA: std::sync::OnceLock<RowSchema> = std::sync::OnceLock::new();
            SCHEMA.get_or_init(RowSchema::default)
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

    #[test]
    fn dropping_cursor_closes_an_unfinished_pipeline() {
        let mut operator = FailingOperator {
            fail_open: false,
            fail_close: false,
            closed: false,
        };
        {
            let _cursor = OperatorBatchCursor::open(&mut operator).unwrap();
        }
        assert!(operator.closed);
    }
}
