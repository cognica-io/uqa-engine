//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep a physical pipeline and its borrowed session inputs in one safe owner, on the caller's thread.

use crate::{Batch, ExecResult, PhysicalOperator, RowSchema};
use std::num::NonZeroUsize;
use uqa_sql::SQLError;

/// The host retains the session, statement snapshot, parameters and lexical scope. Execution builds and drives the existing physical pipeline while that owner remains alive.
pub trait RetainedQueryInput: Send + Sync {
    fn build(&self) -> Result<Option<Box<dyn PhysicalOperator + '_>>, SQLError>;
}

type BoundQuery<'a> = Option<Box<dyn PhysicalOperator + 'a>>;
self_cell::self_cell!(
    struct QueryCell {
        owner: Box<dyn RetainedQueryInput>,
        #[not_covariant]
        dependent: BoundQuery,
    }
);

pub struct RetainedQueryOperator {
    query: QueryCell,
    schema: RowSchema,
    opened: bool,
    ordering: Vec<crate::PhysicalOrder>,
}

impl RetainedQueryOperator {
    pub fn try_new(input: Box<dyn RetainedQueryInput>) -> Result<Option<Self>, SQLError> {
        let query = QueryCell::try_new(input, |input| input.build())?;
        let schema =
            query.with_dependent(|_, input| input.as_ref().map(|input| input.row_schema().clone()));
        let ordering = query.with_dependent(|_, input| {
            input
                .as_ref()
                .map_or_else(Vec::new, |input| input.output_ordering().to_vec())
        });
        Ok(schema.map(|schema| Self {
            query,
            schema,
            opened: false,
            ordering,
        }))
    }
}

impl PhysicalOperator for RetainedQueryOperator {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn estimated_cardinality(&self) -> Option<u64> {
        self.query.with_dependent(|_, input| {
            input
                .as_ref()
                .and_then(|input| input.estimated_cardinality())
        })
    }
    fn output_ordering(&self) -> &[crate::PhysicalOrder] {
        &self.ordering
    }
    fn backward_scan_support(&self) -> crate::BackwardScanSupport {
        self.query.with_dependent(|_, input| {
            input
                .as_ref()
                .expect("retained query present")
                .backward_scan_support()
        })
    }
    fn consume_into_aggregate(
        &mut self,
        executor: &mut dyn crate::relational::AggregateExecutor,
    ) -> ExecResult<bool> {
        self.query.with_dependent_mut(|_, input| {
            input
                .as_mut()
                .expect("retained query present")
                .consume_into_aggregate(executor)
        })
    }
    fn next_direction(
        &mut self,
        direction: crate::PhysicalScanDirection,
    ) -> ExecResult<Option<Batch>> {
        self.query.with_dependent_mut(|_, input| {
            input
                .as_mut()
                .expect("retained query present")
                .next_direction(direction)
        })
    }
    fn rewind(&mut self) -> ExecResult<()> {
        self.query
            .with_dependent_mut(|_, input| input.as_mut().expect("retained query present").rewind())
    }
    fn open(&mut self) -> ExecResult<()> {
        self.opened = true;
        self.query
            .with_dependent_mut(|_, input| input.as_mut().expect("retained query present").open())
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.query
            .with_dependent_mut(|_, input| input.as_mut().expect("retained query present").next())
    }
    fn next_with_demand(&mut self, rows: NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.query.with_dependent_mut(|_, input| {
            input
                .as_mut()
                .expect("retained query present")
                .next_with_demand(rows)
        })
    }
    fn close(&mut self) -> ExecResult<()> {
        if !std::mem::take(&mut self.opened) {
            return Ok(());
        }
        self.query
            .with_dependent_mut(|_, input| input.as_mut().expect("retained query present").close())
    }
}
impl Drop for RetainedQueryOperator {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
