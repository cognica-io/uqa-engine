//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resumable query workers for SQL portals and directional plan inputs.

mod batch;
pub use batch::WorkerBatch;
mod legacy;
pub use legacy::{
    run, SessionPortalWorker, SessionPortalWorkerRequest, SessionPortalWorkerResponse,
};
#[cfg(test)]
mod tests;

use crate::{
    query::consumer::{QueryConsumerControl, QueryRowConsumer},
    query::diagnostics::{InvocationRequest, QueryDiagnostics, RequestScope},
    statement::{context::queries::StatementQueryContexts, thread_context::StatementThreadContext},
    OwnedPhysicalRow, PhysicalScanDirection, RowSchema,
};
use std::{
    cell::{Cell, RefCell},
    num::NonZeroUsize,
    rc::Rc,
    sync::mpsc,
};
use uqa_core::Value;
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam};

#[derive(Default)]
pub enum ResumableQueryRequest {
    Step(PhysicalScanDirection, Option<InvocationRequest>),
    StepBatch {
        direction: PhysicalScanDirection,
        max_rows: NonZeroUsize,
        capture: bool,
        diagnostics: Option<InvocationRequest>,
    },
    Rewind(Option<InvocationRequest>),
    #[default]
    Close,
}

pub enum ResumableQueryResponse<R> {
    Started { schema: RowSchema },
    Row(R),
    Batch(WorkerBatch<R>),
    Eof,
    Rewound,
    Error(SQLError),
}

/// A worker sends its close request before joining, including when its consumer drops early.
pub struct WorkerChannels<Request: Default, Response> {
    pub requests: mpsc::Sender<Request>,
    pub responses: mpsc::Receiver<Response>,
    pub join: Option<std::thread::JoinHandle<()>>,
}

impl<Request: Default + Send + 'static, Response: Send + 'static>
    WorkerChannels<Request, Response>
{
    pub fn spawn(
        run: impl FnOnce(mpsc::Receiver<Request>, mpsc::Sender<Response>) + Send + 'static,
    ) -> Self {
        let (requests, input) = mpsc::channel();
        let (output, responses) = mpsc::channel();
        let join = std::thread::spawn(move || run(input, output));
        Self {
            requests,
            responses,
            join: Some(join),
        }
    }
}

impl<Request: Default, Response> Drop for WorkerChannels<Request, Response> {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::default());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub type ResumableQueryWorker<R> = WorkerChannels<ResumableQueryRequest, ResumableQueryResponse<R>>;

pub(crate) trait WorkerReply<R> {
    fn reply(response: ResumableQueryResponse<R>) -> Self;
}

impl<R> WorkerReply<R> for ResumableQueryResponse<R> {
    fn reply(response: Self) -> Self {
        response
    }
}

#[derive(Clone, Copy)]
pub(crate) enum WorkerOutput {
    Portal { directional: bool },
    SetBranch { width: usize },
}

impl WorkerOutput {
    fn directional(self) -> bool {
        !matches!(self, Self::Portal { directional: false })
    }

    fn buffered_projection_bytes(self, width: usize) -> usize {
        match self {
            // A physical row moves a nonempty positional buffer into a shared fragment.
            Self::SetBranch { .. } if width != 0 => size_of::<Vec<Value>>(),
            _ => 0,
        }
    }
}

struct WorkerRowConsumer<R, Request = ResumableQueryRequest, Response = ResumableQueryResponse<R>> {
    diagnostics: RequestScope,
    requests: mpsc::Receiver<Request>,
    responses: mpsc::Sender<Response>,
    direction: Cell<PhysicalScanDirection>,
    output: WorkerOutput,
    closed: Cell<bool>,
    public_width: Cell<usize>,
    project: fn(Vec<Value>) -> R,
    batch: RefCell<Option<batch::PendingBatch<R>>>,
}

impl<R, Request: Into<ResumableQueryRequest>, Response: WorkerReply<R>>
    WorkerRowConsumer<R, Request, Response>
{
    fn wait_for_request(&self) -> Result<QueryConsumerControl, SQLError> {
        self.apply(
            self.requests
                .recv()
                .map(Into::into)
                .unwrap_or(ResumableQueryRequest::Close),
        )
    }

    fn apply(&self, request: ResumableQueryRequest) -> Result<QueryConsumerControl, SQLError> {
        match request {
            ResumableQueryRequest::Step(direction, diagnostics) => {
                self.diagnostics.replace(diagnostics);
                self.direction.set(direction);
                Ok(QueryConsumerControl::Continue)
            }
            ResumableQueryRequest::StepBatch {
                direction,
                max_rows,
                capture,
                diagnostics,
            } => {
                let control = diagnostics
                    .as_ref()
                    .and_then(InvocationRequest::read_control);
                *self.batch.borrow_mut() =
                    Some(batch::PendingBatch::new(max_rows, capture, control));
                self.diagnostics.replace(diagnostics);
                self.direction.set(direction);
                Ok(QueryConsumerControl::Continue)
            }
            ResumableQueryRequest::Rewind(diagnostics) if self.output.directional() => {
                self.diagnostics.replace(diagnostics);
                Ok(QueryConsumerControl::Rewind)
            }
            ResumableQueryRequest::Rewind(_) => Err(SQLError::Internal(
                "forward-only cursor worker received a rewind request".into(),
            )),
            ResumableQueryRequest::Close => {
                self.closed.set(true);
                Ok(QueryConsumerControl::Stop)
            }
        }
    }

    fn respond_and_wait(
        &self,
        response: ResumableQueryResponse<R>,
    ) -> Result<QueryConsumerControl, SQLError> {
        if self.responses.send(Response::reply(response)).is_err() {
            self.closed.set(true);
            return Ok(QueryConsumerControl::Stop);
        }
        self.wait_for_request()
    }

    fn exhausted(&self) -> ResumableQueryResponse<R> {
        self.batch
            .take()
            .map_or(ResumableQueryResponse::Eof, |batch| {
                ResumableQueryResponse::Batch(batch.finish(None, true))
            })
    }
}

impl<R, Request: Into<ResumableQueryRequest>, Response: WorkerReply<R>> QueryRowConsumer
    for WorkerRowConsumer<R, Request, Response>
{
    fn begin(&self, columns: &[String], schema: &RowSchema) -> Result<(), SQLError> {
        self.public_width.set(columns.len());
        match self.output {
            WorkerOutput::Portal { .. } => {
                let schema = schema.for_result_columns(columns);
                self.responses.send(Response::reply(ResumableQueryResponse::Started { schema }))
                    .map_err(|_| SQLError::Internal("cursor consumer disconnected before startup".into()))
            }
            WorkerOutput::SetBranch { width } if columns.len() != width => Err(SQLError::TypeMismatch(format!(
                "directional set-operation branch width {} does not match declared width {width}", columns.len()
            ))),
            WorkerOutput::SetBranch { .. } => Ok(()),
        }
    }

    fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
        let width = self.public_width.get();
        let response = if let Some(mut batch) = self.batch.take() {
            if !batch.push(
                &row,
                width,
                self.project,
                self.output.buffered_projection_bytes(width),
            )? {
                *self.batch.borrow_mut() = Some(batch);
                return Ok(QueryConsumerControl::Continue);
            }
            let last = batch
                .capture
                .then(|| batch::project(&row, width, self.project));
            ResumableQueryResponse::Batch(batch.finish(last, false))
        } else {
            ResumableQueryResponse::Row(batch::project(&row, width, self.project))
        };
        self.respond_and_wait(response)
    }

    fn uses_directional_scan(&self) -> bool {
        self.output.directional()
    }
    fn scan_direction(&self) -> PhysicalScanDirection {
        self.direction.get()
    }
    fn direction_exhausted(&self) -> Result<QueryConsumerControl, SQLError> {
        self.respond_and_wait(self.exhausted())
    }
    fn rewound(&self) -> Result<QueryConsumerControl, SQLError> {
        self.respond_and_wait(ResumableQueryResponse::Rewound)
    }
}

pub(crate) fn run_resumable<
    R: 'static,
    Request: Into<ResumableQueryRequest> + 'static,
    Response: WorkerReply<R> + 'static,
>(
    thread_context: &StatementThreadContext,
    output: WorkerOutput,
    project: fn(Vec<Value>) -> R,
    requests: mpsc::Receiver<Request>,
    responses: mpsc::Sender<Response>,
    execute: impl FnOnce(Rc<dyn QueryRowConsumer>) -> Result<(), SQLError>,
) {
    let first = loop {
        match requests.recv().map(Into::into) {
            Ok(
                step @ (ResumableQueryRequest::Step(..) | ResumableQueryRequest::StepBatch { .. }),
            ) => break step,
            Ok(ResumableQueryRequest::Rewind(_)) if output.directional() => {
                if responses
                    .send(Response::reply(ResumableQueryResponse::Rewound))
                    .is_err()
                {
                    return;
                }
            }
            _ => return,
        }
    };
    let _statement_thread = thread_context.enter();
    let consumer = Rc::new(WorkerRowConsumer {
        diagnostics: QueryDiagnostics::request_scope(None),
        direction: Cell::new(PhysicalScanDirection::Forward),
        requests,
        responses,
        output,
        project,
        public_width: Cell::new(match output {
            WorkerOutput::Portal { .. } => 0,
            WorkerOutput::SetBranch { width } => width,
        }),
        closed: Cell::new(false),
        batch: RefCell::new(None),
    });
    consumer
        .apply(first)
        .expect("a step admits worker execution");
    let result = execute(consumer.clone());
    if !consumer.closed.get() {
        let response = match (result, output) {
            (Err(error), _) => ResumableQueryResponse::Error(error),
            (Ok(()), WorkerOutput::Portal { .. }) => consumer.exhausted(),
            (Ok(()), WorkerOutput::SetBranch { .. }) => {
                ResumableQueryResponse::Error(SQLError::Internal(
                    "directional query branch stopped without a close request".into(),
                ))
            }
        };
        let _ = consumer.responses.send(Response::reply(response));
    }
}

/// Execute the portal in its retained session when the reader issues its first step.
pub fn run_batched<S: Clone + Send + Sync + 'static>(
    queries: &dyn StatementQueryContexts<S>,
    thread_context: &StatementThreadContext,
    query: &QueryPlan,
    params: &[SQLParam],
    directional: bool,
    requests: mpsc::Receiver<ResumableQueryRequest>,
    responses: mpsc::Sender<ResumableQueryResponse<Vec<Value>>>,
) {
    run_query(
        queries,
        thread_context,
        query,
        params,
        directional,
        requests,
        responses,
    );
}

fn run_query<
    S: Clone + Send + Sync + 'static,
    Request: Into<ResumableQueryRequest> + 'static,
    Response: WorkerReply<Vec<Value>> + 'static,
>(
    queries: &dyn StatementQueryContexts<S>,
    thread_context: &StatementThreadContext,
    query: &QueryPlan,
    params: &[SQLParam],
    directional: bool,
    requests: mpsc::Receiver<Request>,
    responses: mpsc::Sender<Response>,
) {
    run_resumable(
        thread_context,
        WorkerOutput::Portal { directional },
        std::convert::identity,
        requests,
        responses,
        |consumer| {
            let mut ctes = queries.statement_scope(None)?;
            ctes.enable_command_progress_streaming();
            if directional {
                ctes.enable_backwards_scanning();
            }
            crate::query::statement::execute_query_plan_output(
                &queries.query_context(),
                query,
                params,
                &mut ctes,
                crate::query::statement::consumer::QueryOutputMode::physical_consumer(consumer),
            )
            .map(|_| ())
        },
    );
}
