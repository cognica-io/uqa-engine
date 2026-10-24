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
    closed: bool,
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
            closed: false,
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
        self.closed = false;
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
        if std::mem::replace(&mut self.closed, true) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct Input(Arc<AtomicUsize>);
    struct Source(Arc<AtomicUsize>);

    impl crate::SQLRowSource<crate::PhysicalRow> for Source {
        fn next_row(&mut self) -> Result<Option<crate::PhysicalRow>, SQLError> {
            panic!("closing must not fetch a row")
        }
        fn close(&mut self) -> Result<(), SQLError> {
            assert_eq!(self.0.fetch_add(1, Ordering::SeqCst), 0);
            Err(SQLError::Routine {
                sqlstate: "58030".into(),
                message: "retained scan close failed".into(),
            })
        }
    }

    impl RetainedQueryInput for Input {
        fn build(&self) -> Result<Option<Box<dyn PhysicalOperator + '_>>, SQLError> {
            Ok(Some(Box::new(crate::PhysicalRowIteratorScan::from_stream(
                RowSchema::new(vec!["n".into()]),
                crate::SQLRowStream::from_source(Source(self.0.clone())),
            ))))
        }
    }

    #[test]
    fn retained_pipeline_close_reports_resources_created_before_open() {
        for open in [false, true] {
            let closed = Arc::new(AtomicUsize::new(0));
            let mut pipeline = RetainedQueryOperator::try_new(Box::new(Input(closed.clone())))
                .unwrap()
                .unwrap();
            if open {
                pipeline.open().unwrap();
            }
            let error = crate::physical::physical_exec_error(pipeline.close().unwrap_err());
            assert_eq!(error.sqlstate(), Some("58030"));
            pipeline.close().unwrap();
            drop(pipeline);
            assert_eq!(closed.load(Ordering::SeqCst), 1);
        }
    }
}
