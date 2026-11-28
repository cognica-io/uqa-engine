//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent directional query traversal over a session-owned worker task.
use crate::query::{consumer::QueryRowConsumer, diagnostics::QueryDiagnostics};
use crate::statement::portal::worker::{
    run_resumable, ResumableQueryRequest, ResumableQueryResponse, ResumableQueryWorker,
    WorkerOutput,
};
use crate::{
    BackwardScanSupport, Batch, ExecError, ExecResult, PhysicalOperator, PhysicalRow,
    PhysicalScanDirection, RowSchema,
};
use std::{num::NonZeroUsize, rc::Rc};
use uqa_core::memory::{MemoryReservation, ProductionControl};
use uqa_sql::SQLError;

/// Run one query in the session state prepared by the caller. The session owns its statement gate and snapshot lifetime.
pub trait DirectionalQueryTask: Send {
    fn execute(self: Box<Self>, consumer: Rc<dyn QueryRowConsumer>) -> Result<(), SQLError>;
}

pub struct DirectionalQueryPlanOperator {
    diagnostics: Option<QueryDiagnostics>,
    schema: RowSchema,
    support: BackwardScanSupport,
    input: Option<Box<dyn DirectionalQueryTask>>,
    worker: Option<ResumableQueryWorker<PhysicalRow>>,
    _memory: Option<MemoryReservation>,
}

impl DirectionalQueryPlanOperator {
    pub fn new_with_control(
        input: Box<dyn DirectionalQueryTask>,
        support: BackwardScanSupport,
        schema: RowSchema,
        control: &ProductionControl<'_>,
    ) -> Result<Box<Self>, SQLError> {
        let memory = control.reserve(size_of::<Self>())?;
        Ok(Box::new(Self {
            diagnostics: None,
            schema,
            support,
            input: Some(input),
            worker: None,
            _memory: memory,
        }))
    }

    #[must_use]
    pub fn with_diagnostics(mut self: Box<Self>, diagnostics: QueryDiagnostics) -> Box<Self> {
        self.diagnostics = Some(diagnostics);
        self
    }

    fn request(
        &mut self,
        request: ResumableQueryRequest,
    ) -> ExecResult<ResumableQueryResponse<PhysicalRow>> {
        if self.worker.is_none() {
            let input = self
                .input
                .take()
                .ok_or_else(|| ExecError::Other("directional query branch is closed".into()))?;
            let output = WorkerOutput::SetBranch {
                width: self.schema.len(),
            };
            let context = crate::statement::thread_context::StatementThreadContext::current();
            self.worker = Some(ResumableQueryWorker::spawn(move |requests, responses| {
                run_resumable(
                    &context,
                    output,
                    PhysicalRow::from_values,
                    requests,
                    responses,
                    |consumer| input.execute(consumer),
                );
            }));
        }
        let worker = self
            .worker
            .as_ref()
            .expect("directional worker was started");
        worker.requests.send(request).map_err(|_| {
            ExecError::Other("directional query branch stopped before a request".into())
        })?;
        worker.responses.recv().map_err(|_| {
            ExecError::Other("directional query branch stopped without a response".into())
        })
    }

    fn pull(
        &mut self,
        direction: PhysicalScanDirection,
        demand: NonZeroUsize,
    ) -> ExecResult<Option<Batch>> {
        let request = ResumableQueryRequest::StepBatch {
            direction,
            max_rows: demand,
            capture: true,
            diagnostics: self.diagnostics.as_ref().map(QueryDiagnostics::request),
        };
        match self.request(request)? {
            ResumableQueryResponse::Batch(batch) if batch.processed == 0 => Ok(None),
            ResumableQueryResponse::Batch(batch) => {
                let mut rows = Vec::new();
                batch.append_to(&mut rows);
                Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)))
            }
            ResumableQueryResponse::Error(error) => Err(ExecError::SQL(error)),
            _ => Err(ExecError::Other(
                "directional query branch returned an invalid row response".into(),
            )),
        }
    }
}

impl PhysicalOperator for DirectionalQueryPlanOperator {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn backward_scan_support(&self) -> BackwardScanSupport {
        self.support
    }

    fn open(&mut self) -> ExecResult<()> {
        if self.input.is_some() || self.worker.is_some() {
            Ok(())
        } else {
            Err(ExecError::Other(
                "directional query branch is closed".into(),
            ))
        }
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.next_direction(PhysicalScanDirection::Forward)
    }

    fn next_direction(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<Batch>> {
        self.pull(direction, crate::physical::demand::ONE)
    }

    fn next_with_demand(&mut self, rows: NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.pull(PhysicalScanDirection::Forward, rows)
    }

    fn rewind(&mut self) -> ExecResult<()> {
        if self.input.is_some() {
            return Ok(());
        }
        match self.request(ResumableQueryRequest::Rewind(
            self.diagnostics.as_ref().map(QueryDiagnostics::request),
        ))? {
            ResumableQueryResponse::Rewound => Ok(()),
            ResumableQueryResponse::Error(error) => Err(ExecError::SQL(error)),
            _ => Err(ExecError::Other(
                "directional query branch returned an invalid rewind response".into(),
            )),
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.worker = None;
        self.input = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
