//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Blocking hash/sort aggregation and aggregate folds.

use super::{
    compare_values, eval_scalar, Batch, DefaultExpressionEvaluator, ExecError, ExecResult,
    PhysicalOperator, RowSchema, SQLParam, ScalarEvalContext, ScalarExpr, SortKey, Value,
};
use crate::ProjectedRow;

mod adaptive;
mod fold;
mod partial;
mod sort_fallback;

pub(super) use fold::value_to_f64;
#[cfg(test)]
pub(super) use fold::{finalise_fold, AggFold};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    Count,
    CountStar,
    Sum,
    Avg,
    Min,
    Max,
}

#[derive(Debug, Clone)]
pub struct AggregateSpec {
    pub kind: AggregateKind,
    /// Argument to the aggregate. Ignored for `CountStar`.
    pub arg: Option<ScalarExpr>,
    /// Output column alias.
    pub alias: String,
    /// `COUNT(DISTINCT x)` / `SUM(DISTINCT x)` / etc.
    pub distinct: bool,
}

/// Blocking group-by + aggregate. Pulls every row from the child
/// during `open`, hashes each row by its group key, and folds the
/// aggregates over each group's row set. Groups are emitted in the
/// order they were first observed.
pub trait AggregateExecutor: Send {
    /// Consume one child batch. Implementations that need a blocking input must
    /// enforce their own byte budget here; the physical operator never creates
    /// an unbounded intermediate row vector.
    fn consume(&mut self, batch: Batch) -> ExecResult<()>;

    /// Whether this executor can fold a borrowed, positional row without a
    /// materialized `ResultRow`. A source checks this before advancing.
    fn supports_projected_rows(&self) -> bool {
        false
    }

    /// Fold one projected row. Implementations advertising support must
    /// preserve the same expression and aggregate semantics as [`Self::consume`].
    fn consume_projected_row(&mut self, _row: &ProjectedRow<'_, '_>) -> ExecResult<()> {
        Err(ExecError::Other(
            "aggregate executor does not accept projected rows".into(),
        ))
    }

    /// Finalize all groups into a byte-bounded, disk-backed output stream.
    /// The row-oriented SQL API may materialize that stream at its public API
    /// boundary, but physical operators must not create an unbounded result
    /// vector first.
    fn finish(&mut self) -> ExecResult<crate::spill::SpillBuffer>;
}

pub struct HashAggregate<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    group_keys: Vec<(String, ScalarExpr)>,
    aggregates: Vec<AggregateSpec>,
    params: Vec<SQLParam>,
    schema: RowSchema,
    executor: Option<Box<dyn AggregateExecutor + 'a>>,
    work_mem_bytes: usize,
    output: Option<crate::spill::SpillDrain>,
    output_spilled: bool,
}

impl HashAggregate<'static> {
    const DEFAULT_WORK_MEM_BYTES: usize = 64 * 1024 * 1024;

    pub fn new(
        child: Box<dyn PhysicalOperator>,
        group_keys: Vec<(String, ScalarExpr)>,
        aggregates: Vec<AggregateSpec>,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::new_with_work_mem(
            child,
            group_keys,
            aggregates,
            params,
            Self::DEFAULT_WORK_MEM_BYTES,
        )
    }

    pub fn new_with_work_mem(
        child: Box<dyn PhysicalOperator>,
        group_keys: Vec<(String, ScalarExpr)>,
        aggregates: Vec<AggregateSpec>,
        params: Vec<SQLParam>,
        work_mem_bytes: usize,
    ) -> Self {
        let mut cols: Vec<String> = group_keys.iter().map(|(n, _)| n.clone()).collect();
        for a in &aggregates {
            cols.push(a.alias.clone());
        }
        let schema = RowSchema::new(cols);
        Self {
            child,
            group_keys,
            aggregates,
            params,
            schema,
            executor: None,
            work_mem_bytes,
            output: None,
            output_spilled: false,
        }
    }
}

impl<'a> HashAggregate<'a> {
    /// Construct a physical aggregate backed by the engine's full aggregate
    /// registry. Input is delivered incrementally through
    /// [`AggregateExecutor::consume`].
    pub fn with_executor(
        child: Box<dyn PhysicalOperator + 'a>,
        output_schema: Vec<String>,
        executor: Box<dyn AggregateExecutor + 'a>,
    ) -> Self {
        let types = vec![None; output_schema.len()];
        Self::with_typed_executor(child, output_schema, types, executor)
    }

    pub fn with_typed_executor(
        child: Box<dyn PhysicalOperator + 'a>,
        output_schema: Vec<String>,
        output_types: Vec<Option<uqa_sql::ast::ColumnType>>,
        executor: Box<dyn AggregateExecutor + 'a>,
    ) -> Self {
        Self {
            child,
            group_keys: Vec::new(),
            aggregates: Vec::new(),
            params: Vec::new(),
            schema: RowSchema::with_types(output_schema, output_types),
            executor: Some(executor),
            work_mem_bytes: 0,
            output: None,
            output_spilled: false,
        }
    }

    /// Whether final aggregate rows exceeded their output budget and were
    /// written to disk during the current/most recent invocation.
    pub fn output_has_spilled(&self) -> bool {
        self.output_spilled
    }
}

impl PhysicalOperator for HashAggregate<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()?;
        self.output_spilled = false;
        if let Some(executor) = self.executor.as_mut() {
            let consumed_directly = executor.supports_projected_rows()
                && self.child.consume_into_aggregate(executor.as_mut())?;
            if !consumed_directly {
                while let Some(batch) = self.child.next()? {
                    executor.consume(batch)?;
                }
            }
            let mut output = executor.finish()?;
            self.output_spilled = output.has_spilled();
            self.output = Some(output.drain()?);
            return Ok(());
        }
        let mut output = if adaptive::supported(&self.group_keys, &self.aggregates) {
            let mut aggregate = adaptive::AdaptiveBuiltinAggregate::new(
                &self.group_keys,
                &self.aggregates,
                &self.params,
                self.work_mem_bytes,
            );
            while let Some(batch) = self.child.next()? {
                aggregate.consume(batch)?;
            }
            aggregate.finish(self.schema.clone())?
        } else {
            sort_fallback::execute(
                self.child.as_mut(),
                &self.group_keys,
                &self.aggregates,
                &self.params,
                self.schema.clone(),
                self.work_mem_bytes,
            )?
        };
        self.output_spilled = output.has_spilled();
        self.output = Some(output.drain()?);
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(output) = self.output.as_mut() else {
            return Ok(None);
        };
        output.next().transpose()
    }

    fn close(&mut self) -> ExecResult<()> {
        self.output = None;
        self.child.close()
    }
}

// -------------------------------------------------------------------------
// Window
// -------------------------------------------------------------------------
