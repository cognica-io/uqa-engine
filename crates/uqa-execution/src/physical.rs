//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volcano `PhysicalOperator` trait shared by every operator in this
//! crate. Mirrors `uqa/execution/physical.py`.

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
    op.open()?;
    let mut out = Vec::new();
    while let Some(batch) = op.next()? {
        out.push(batch);
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
