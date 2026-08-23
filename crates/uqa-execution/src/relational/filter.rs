//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pipelined WHERE filtering.

use super::{
    truthy, Batch, DefaultExpressionEvaluator, ExecResult, PhysicalOperator, RowSchema, SQLParam,
    ScalarExpr, SharedExpressionEvaluator, SharedRowPredicate,
};

/// Pipelined `WHERE` operator. Drops rows whose predicate evaluates
/// to `false` or `NULL`; truthy rows pass through unchanged.
pub struct Filter<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    condition: FilterCondition<'a>,
    schema: RowSchema,
}

enum FilterCondition<'a> {
    Expression {
        predicate: ScalarExpr,
        evaluator: SharedExpressionEvaluator<'a>,
    },
    Row(SharedRowPredicate<'a>),
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
            condition: FilterCondition::Expression {
                predicate,
                evaluator,
            },
            schema,
        }
    }

    pub fn with_row_predicate(
        child: Box<dyn PhysicalOperator + 'a>,
        predicate: SharedRowPredicate<'a>,
    ) -> Self {
        let schema = child.row_schema().clone();
        Self {
            child,
            condition: FilterCondition::Row(predicate),
            schema,
        }
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

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        loop {
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            let mut kept = Vec::with_capacity(batch.rows.len());
            for row in batch.rows {
                let keep = match &self.condition {
                    FilterCondition::Expression {
                        predicate,
                        evaluator,
                    } => truthy(&evaluator.evaluate_physical(predicate, &batch.schema, &row)?),
                    FilterCondition::Row(predicate) => {
                        predicate.keep_physical(&batch.schema, &row)?
                    }
                };
                if keep {
                    kept.push(row);
                }
            }
            if !kept.is_empty() {
                return Ok(Some(Batch::from_physical_rows(self.schema.clone(), kept)));
            }
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}
