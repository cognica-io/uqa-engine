//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pipelined WHERE filtering.

use super::{
    truthy, BackwardScanSupport, Batch, DefaultExpressionEvaluator, ExecError, ExecResult,
    PhysicalOperator, PhysicalScanDirection, RowSchema, SQLParam, ScalarExpr,
    SharedExpressionEvaluator, SharedRowPredicate,
};

use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{plan::OwnedScalarExpr, SQLError};

/// Pipelined `WHERE` operator. Drops rows whose predicate evaluates
/// to `false` or `NULL`; truthy rows pass through unchanged.
pub struct Filter<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    input_pending: crate::physical::demand::PendingRows,
    condition: FilterCondition<'a>,
    schema: RowSchema,
    _memory: Option<uqa_core::memory::MemoryReservation>,
}

enum FilterCondition<'a> {
    Expression {
        predicate: crate::scalar::PreparedExpressions<Produced<ScalarExpr>>,
        evaluator: SharedExpressionEvaluator<'a>,
    },
    Row(SharedRowPredicate<'a>),
    /// A predicate that names a column its input does not produce. Reading that column would yield NULL for every row, so the first input row reports the defect instead.
    Unresolved(Produced<String>),
}

impl Filter<'static> {
    pub fn new(
        child: Box<dyn PhysicalOperator>,
        predicate: ScalarExpr,
        params: Vec<SQLParam>,
    ) -> Self {
        Self::with_evaluator(child, predicate, DefaultExpressionEvaluator::shared(params))
    }
}

impl<'a> Filter<'a> {
    pub fn with_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        predicate: ScalarExpr,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        let schema = child.row_schema().clone();
        let predicate = evaluator.bind_type_introspection(predicate, &schema);
        Self {
            child,
            input_pending: crate::physical::demand::PendingRows::default(),
            condition: FilterCondition::Expression {
                predicate: crate::scalar::PreparedExpressions::produced_scalar(
                    ProductionControl::uncontrolled()
                        .finish(predicate, None)
                        .expect("ordinary predicate"),
                    &ProductionControl::uncontrolled(),
                )
                .expect("ordinary predicate preparation"),
                evaluator,
            },
            schema,
            _memory: None,
        }
    }

    pub fn with_row_predicate(
        child: Box<dyn PhysicalOperator + 'a>,
        predicate: SharedRowPredicate<'a>,
    ) -> Self {
        let schema = child.row_schema().clone();
        Self {
            child,
            input_pending: crate::physical::demand::PendingRows::default(),
            condition: FilterCondition::Row(predicate),
            schema,
            _memory: None,
        }
    }

    /// A filter whose predicate reads `column`, which `child` does not produce. Statement validation reports a user's unknown column before execution, so a row reaching this filter exposes a predicate that was placed on the wrong input; it fails with an internal error instead of reading NULL.
    pub fn unresolved(child: Box<dyn PhysicalOperator + 'a>, column: String) -> Self {
        let schema = child.row_schema().clone();
        Self {
            child,
            input_pending: crate::physical::demand::PendingRows::default(),
            condition: FilterCondition::Unresolved(
                ProductionControl::uncontrolled()
                    .finish(column, None)
                    .expect("ordinary unresolved column"),
            ),
            schema,
            _memory: None,
        }
    }

    /// Prepare the admitted predicate while retaining its original child on every failure.
    pub fn with_owned_predicate(
        child: Box<dyn PhysicalOperator + 'a>,
        predicate: OwnedScalarExpr,
        evaluator: SharedExpressionEvaluator<'a>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let (child, (schema, predicate)) =
            crate::physical::construction::OperatorBuild::run(child, |child| {
                let schema = child.row_schema().clone();
                let predicate =
                    evaluator.bind_type_introspection_owned(predicate, &schema, control)?;
                let predicate = crate::scalar::PreparedExpressions::produced_scalar(
                    predicate.finish(control)?,
                    control,
                )?;
                Ok((schema, predicate))
            })?;
        Ok(Self {
            child,
            input_pending: crate::physical::demand::PendingRows::default(),
            condition: FilterCondition::Expression {
                predicate,
                evaluator,
            },
            schema,
            _memory: None,
        })
    }

    /// Keep a placed-column diagnostic deferred until its input actually produces a row.
    pub fn with_unresolved_column(
        child: Box<dyn PhysicalOperator + 'a>,
        column: Produced<String>,
    ) -> Self {
        let schema = child.row_schema().clone();
        Self {
            child,
            input_pending: crate::physical::demand::PendingRows::default(),
            condition: FilterCondition::Unresolved(column),
            schema,
            _memory: None,
        }
    }

    pub fn into_boxed(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
        let Self {
            child,
            input_pending,
            condition,
            schema,
            _memory: _,
        } = self;
        crate::physical::construction::OperatorBuild::build(child, |owner| {
            let memory = control.reserve(size_of::<Self>())?;
            owner.wrap(|child| {
                Box::new(Self {
                    child,
                    input_pending,
                    condition,
                    schema,
                    _memory: memory,
                })
            });
            Ok(())
        })
    }

    fn filter_batch(&self, batch: Batch) -> ExecResult<Option<Batch>> {
        let mut kept = Vec::with_capacity(batch.rows.len());
        for row in batch.rows {
            let keep = match &self.condition {
                FilterCondition::Expression {
                    predicate,
                    evaluator,
                } => truthy(&evaluator.evaluate_physical_with_function_states(
                    predicate,
                    &batch.schema,
                    &row,
                    predicate.calls(),
                )?),
                FilterCondition::Row(predicate) => predicate.keep_physical(&batch.schema, &row)?,
                FilterCondition::Unresolved(column) => {
                    return Err(ExecError::SQL(uqa_sql::SQLError::Internal(format!(
                        "a predicate placed on its input reads {}, which the input does not produce", column.as_str()
                    ))));
                }
            };
            if keep {
                kept.push(row);
            }
        }
        Ok((!kept.is_empty()).then(|| Batch::from_physical_rows(self.schema.clone(), kept)))
    }
}

impl Filter<'_> {
    fn pull(&mut self, demand: Option<std::num::NonZeroUsize>) -> ExecResult<Option<Batch>> {
        let Some(demand) = demand else {
            loop {
                let Some(batch) = self.input_pending.pull(self.child.as_mut(), None)? else {
                    return Ok(None);
                };
                if let Some(batch) = self.filter_batch(batch)? {
                    return Ok(Some(batch));
                }
            }
        };
        let mut kept = Vec::new();
        while kept.len() < demand.get() {
            let Some(batch) = self
                .input_pending
                .pull(self.child.as_mut(), Some(crate::physical::demand::ONE))?
            else {
                break;
            };
            if let Some(batch) = self.filter_batch(batch)? {
                kept.extend(batch.rows);
            }
        }
        Ok((!kept.is_empty()).then(|| Batch::from_physical_rows(self.schema.clone(), kept)))
    }
}

impl PhysicalOperator for Filter<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.child.estimated_cardinality()
    }

    fn output_ordering(&self) -> &[crate::PhysicalOrder] {
        self.child.output_ordering()
    }

    fn backward_scan_support(&self) -> BackwardScanSupport {
        if self.child.backward_scan_support() == BackwardScanSupport::Native {
            BackwardScanSupport::Native
        } else {
            BackwardScanSupport::Unsupported
        }
    }

    fn open(&mut self) -> ExecResult<()> {
        self.input_pending.clear();
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.pull(None)
    }

    fn next_with_demand(&mut self, rows: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.pull(Some(rows))
    }

    fn next_direction(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<Batch>> {
        loop {
            let Some(batch) = self.child.next_direction(direction)? else {
                return Ok(None);
            };
            if let Some(batch) = self.filter_batch(batch)? {
                return Ok(Some(batch));
            }
        }
    }

    fn rewind(&mut self) -> ExecResult<()> {
        self.input_pending.clear();
        self.child.rewind()
    }

    fn close(&mut self) -> ExecResult<()> {
        self.input_pending.clear();
        self.child.close()
    }
}

#[cfg(test)]
mod production_tests;
